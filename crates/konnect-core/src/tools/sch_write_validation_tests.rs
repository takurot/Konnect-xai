//! #132: schematic write tools refuse values KiCad would misread, and adding
//! an item that is already there is a no-op.

use super::*;
use crate::mcp::protocol::ToolContent;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn symbol(reference: &str, uuid: &str, x: f64, y: f64) -> String {
    format!(
        "\t(symbol (lib_id \"Device:R\") (at {x} {y} 0) (unit 1) (uuid \"{uuid}\")\n\
         \t\t(property \"Reference\" \"{reference}\" (at {x} {y} 0))\n\
         \t\t(property \"Value\" \"10k\" (at {x} {y} 0))\n\
         \t\t(instances (project \"s\" (path \"/root\" (reference \"{reference}\") (unit 1))))\n\
         \t)\n"
    )
}

/// R1's pins are free at (101.6, 97.79) and (101.6, 105.41). R2's pin 1 at
/// (127, 97.79) ends a wire. R3's pin 1 at (152.4, 97.79) sits mid-wire. The
/// wire along y = 127 is empty for junctions.
fn sheet_source() -> String {
    format!(
        "(kicad_sch\n\t(version 20250610)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n\t(paper \"A4\")\n\
         \t(lib_symbols\n\t\t(symbol \"Device:R\"\n\t\t\t(symbol \"R_1_1\"\n\
         \t\t\t\t(pin passive line (at 0 3.81 270) (length 1.27) (name \"~\") (number \"1\"))\n\
         \t\t\t\t(pin passive line (at 0 -3.81 90) (length 1.27) (name \"~\") (number \"2\"))\n\
         \t\t\t)\n\t\t)\n\t)\n\
         \t(wire (pts (xy 127 97.79) (xy 127 88.9)) (uuid \"w-r2\"))\n\
         \t(wire (pts (xy 146.05 97.79) (xy 158.75 97.79)) (uuid \"w-r3\"))\n\
         \t(wire (pts (xy 101.6 127) (xy 127 127)) (uuid \"w-free\"))\n\
         {}{}{}\
         \t(sheet_instances (path \"/\" (page \"1\")))\n)\n",
        symbol("R1", "u-r1", 101.6, 101.6),
        symbol("R2", "u-r2", 127.0, 101.6),
        symbol("R3", "u-r3", 152.4, 101.6),
    )
}

fn fixture(source: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.kicad_sch");
    std::fs::write(&path, source).unwrap();
    (dir, path)
}

fn all_tools() -> Vec<ToolDef> {
    [
        sch_wiring::tools(),
        sch_components::tools(),
        sch_batch::tools(),
        sch_bus::tools(),
        sch_export::tools(),
        sch_hierarchy::tools(),
        templates::tools(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

async fn call(tool: &str, path: &Path, mut args: Value) -> CallToolResult {
    args["schematic"] = json!(path.display().to_string());
    let definition = all_tools()
        .into_iter()
        .find(|definition| definition.name == tool)
        .unwrap_or_else(|| panic!("{tool} is registered"));
    let context = Arc::new(ToolContext::new(
        ServerConfig::default(),
        Arc::new(crate::router::ToolRouter::new()),
    ));
    (definition.handler)(&args, context).await.unwrap()
}

fn body(result: &CallToolResult) -> Value {
    match result.content.first() {
        Some(ToolContent::Text { text }) => {
            serde_json::from_str(text).unwrap_or_else(|_| json!({ "text": text }))
        }
        other => panic!("expected text, got {other:?}"),
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// The call is refused as `kind` naming `field`, and the file is untouched.
async fn assert_refused(tool: &str, path: &Path, args: Value, kind: &str, field: Option<&str>) {
    let before = read(path);
    let result = call(tool, path, args.clone()).await;
    assert!(result.is_error, "{tool} accepted {args}: {result:?}");
    let error = &body(&result)["error"];
    assert_eq!(error["kind"], kind, "{tool} {args}: {error}");
    if let Some(field) = field {
        assert_eq!(error["field"], field, "{tool} {args}: {error}");
    }
    assert_eq!(read(path), before, "{tool} {args} changed the file");
}

fn count(path: &Path, head: &str) -> usize {
    read(path).matches(&format!("({head}")).count()
}

#[tokio::test]
async fn components_refuse_coordinates_kicad_would_clamp() {
    let (_dir, path) = fixture(&sheet_source());
    assert_refused(
        "add_schematic_component",
        &path,
        json!({ "lib_id": "Device:R", "x": 300000.0, "y": 50.0 }),
        "invalid_argument",
        Some("x"),
    )
    .await;
    assert_refused(
        "move_schematic_component",
        &path,
        json!({ "reference": "R1", "x": -250000.0, "y": 50.0 }),
        "invalid_argument",
        Some("x"),
    )
    .await;
    // The origin is in range; pin 2, 3.81 mm below it, is not.
    assert_refused(
        "move_schematic_component",
        &path,
        json!({ "reference": "R1", "x": 101.6, "y": 151847.6 }),
        "invalid_argument",
        Some("y"),
    )
    .await;
}

async fn sheet_with_right_and_top_pins() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("root.kicad_sch");
    std::fs::write(&path, blank_schematic_template_with_paper("A4", false)).unwrap();
    let added = call(
        "add_hierarchical_sheet",
        &path,
        json!({ "sheet_file": "a.kicad_sch", "sheet_name": "A" }),
    )
    .await;
    assert!(!added.is_error, "{added:?}");
    for (name, x, y, side) in [
        ("RIGHT", 130.0, 60.96, "right"),
        ("TOP", 120.65, 50.0, "top"),
    ] {
        let pin = call(
            "add_sheet_pin",
            &path,
            json!({ "sheet_name": "A", "pin_name": name, "pin_type": "input",
                    "x": x, "y": y, "side": side }),
        )
        .await;
        assert!(!pin.is_error, "{pin:?}");
    }
    (dir, path)
}

#[tokio::test]
async fn edit_sheet_refuses_a_resize_that_leaves_a_pin_off_its_edge() {
    let (_dir, path) = sheet_with_right_and_top_pins().await;
    assert_refused(
        "edit_sheet",
        &path,
        json!({ "sheet_name": "A", "width": 12.7, "height": 5.08 }),
        "invalid_argument",
        Some("width"),
    )
    .await;
    // Only the width shrinks: TOP at x = 120.65 is past the new right corner.
    assert_refused(
        "edit_sheet",
        &path,
        json!({ "sheet_name": "A", "width": 60.96, "height": 50.0 }),
        "invalid_argument",
        Some("width"),
    )
    .await;
    let flagged = call("add_no_connect", &path, json!({ "x": 130.0, "y": 60.96 })).await;
    assert!(
        !flagged.is_error,
        "an unused sheet pin takes a flag: {flagged:?}"
    );
    let taller = call(
        "edit_sheet",
        &path,
        json!({ "sheet_name": "A", "width": 80.0, "height": 60.96 }),
    )
    .await;
    assert!(!taller.is_error, "pins stay on their edges: {taller:?}");
}

#[tokio::test]
async fn add_no_connect_only_marks_a_free_pin_and_is_idempotent() {
    let (_dir, path) = fixture(&sheet_source());
    for (x, y, why) in [
        (200.66, 200.66, "nothing there"),
        (127.0, 97.79, "wired pin"),
        (152.4, 97.79, "pin on a wire's interior"),
    ] {
        let before = read(&path);
        let result = call("add_no_connect", &path, json!({ "x": x, "y": y })).await;
        assert!(result.is_error, "{why}: {result:?}");
        assert_eq!(body(&result)["error"]["kind"], "invalid_argument", "{why}");
        assert_eq!(read(&path), before, "{why}");
    }

    let first = call("add_no_connect", &path, json!({ "x": 101.6, "y": 97.79 })).await;
    assert!(!first.is_error, "{first:?}");
    assert_eq!(body(&first)["already_present"], false);
    let after_first = read(&path);
    let again = call("add_no_connect", &path, json!({ "x": 101.6, "y": 97.79 })).await;
    assert!(!again.is_error, "{again:?}");
    assert_eq!(body(&again)["already_present"], true);
    assert_eq!(read(&path), after_first);
    assert_eq!(count(&path, "no_connect"), 1);
}

#[tokio::test]
async fn batch_add_no_connect_reports_refused_positions_and_collapses_duplicates() {
    let (_dir, path) = fixture(&sheet_source());
    let result = call(
        "batch_add_no_connect",
        &path,
        json!({ "positions": [{ "x": 101.6, "y": 97.79 }, { "x": 127.0, "y": 97.79 },
                              { "x": 101.6, "y": 97.79 }, { "x": 101.6, "y": 105.41 }] }),
    )
    .await;
    assert!(!result.is_error, "{result:?}");
    let body = body(&result);
    assert_eq!(body["added_count"], 2, "{body}");
    assert_eq!(body["already_present_count"], 1, "{body}");
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{body}");
    assert!(
        errors[0]
            .as_str()
            .unwrap()
            .starts_with("Position 1: pin R2.1"),
        "{body}"
    );
    assert_eq!(count(&path, "no_connect"), 2);
}

#[tokio::test]
async fn add_junction_needs_a_wire_and_is_idempotent() {
    let (_dir, path) = fixture(&sheet_source());
    assert_refused(
        "add_junction",
        &path,
        json!({ "x": 200.66, "y": 200.66 }),
        "invalid_argument",
        Some("x"),
    )
    .await;
    assert_refused(
        "add_junction",
        &path,
        json!({ "x": 114.3 }),
        "invalid_argument",
        Some("y"),
    )
    .await;
    let first = call("add_junction", &path, json!({ "x": 114.3, "y": 127.0 })).await;
    assert!(!first.is_error, "{first:?}");
    assert_eq!(body(&first)["already_present"], false);
    let after_first = read(&path);
    let again = call("add_junction", &path, json!({ "x": 114.3, "y": 127.0 })).await;
    assert!(!again.is_error, "{again:?}");
    assert_eq!(body(&again)["already_present"], true);
    assert_eq!(read(&path), after_first);
}

#[tokio::test]
async fn batch_add_junction_collapses_duplicates_and_refuses_bad_points_atomically() {
    let (_dir, path) = fixture(&sheet_source());
    for (positions, field) in [
        (
            json!([{ "x": 114.3, "y": 127.0 }, { "x": 200.66, "y": 200.66 }]),
            "positions[1]",
        ),
        (json!([{ "x": 114.3 }]), "positions[0]"),
    ] {
        assert_refused(
            "batch_add_junction",
            &path,
            json!({ "positions": positions }),
            "invalid_argument",
            Some(field),
        )
        .await;
    }
    let result = call(
        "batch_add_junction",
        &path,
        json!({ "positions": [{ "x": 114.3, "y": 127.0 }, { "x": 114.3, "y": 127.0 },
                              { "x": 116.84, "y": 127.0 }] }),
    )
    .await;
    assert!(!result.is_error, "{result:?}");
    assert_eq!(body(&result)["added"], 2);
    assert_eq!(body(&result)["already_present"], 1);
    assert_eq!(count(&path, "junction"), 2);
}

#[tokio::test]
async fn net_names_must_be_visible_text() {
    let (_dir, path) = fixture(&sheet_source());
    for net in ["", "  ", "A\nB"] {
        assert_refused(
            "add_schematic_net_label",
            &path,
            json!({ "net": net, "x": 114.3, "y": 127.0 }),
            "invalid_argument",
            Some("net"),
        )
        .await;
        assert_refused(
            "connect_passthrough",
            &path,
            json!({ "net_name": net, "x": 101.6, "y": 97.79 }),
            "invalid_argument",
            Some("net_name"),
        )
        .await;
    }
}

#[tokio::test]
async fn lengths_and_sizes_must_be_positive() {
    let (_dir, path) = fixture(&sheet_source());
    for stub_length in [-5.0, 0.0] {
        assert_refused(
            "connect_to_net",
            &path,
            json!({ "net": "N", "reference": "R1", "pin_number": "1",
                    "direction": "right", "stub_length": stub_length }),
            "invalid_argument",
            Some("stub_length"),
        )
        .await;
    }
    assert_refused(
        "add_bus",
        &path,
        json!({ "x1": 50.8, "y1": 50.8, "x2": 50.8, "y2": 50.8 }),
        "invalid_argument",
        None,
    )
    .await;
    for size in [0.0, -1.27] {
        assert_refused(
            "add_schematic_text",
            &path,
            json!({ "text": "note", "x": 50.8, "y": 50.8, "size": size }),
            "invalid_argument",
            Some("size"),
        )
        .await;
    }
}

/// A wire whose loose end (50, 80) is 0.5 mm from a label. `extra` adds
/// whatever the other end or the neighbourhood needs.
fn near_miss_source(extra: &str) -> String {
    format!(
        "(kicad_sch\n\t(version 20250610)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n\t(lib_symbols)\n\
         \t(wire (pts (xy 40 80) (xy 50 80)) (uuid \"w1\"))\n\
         \t(label \"N2\" (at 50.5 80 0) (uuid \"l2\"))\n{extra}\
         \t(sheet_instances (path \"/\" (page \"1\")))\n)\n"
    )
}

#[tokio::test]
async fn fix_connectivity_bounds_its_tolerance_and_refuses_net_changes() {
    let (_dir, path) = fixture(&near_miss_source(""));
    for tolerance in [11.0, 1.28, 0.0, -1.0] {
        assert_refused(
            "fix_connectivity",
            &path,
            json!({ "snap_tolerance": tolerance }),
            "invalid_argument",
            Some("snap_tolerance"),
        )
        .await;
    }

    let (_dir, path) = fixture(&near_miss_source(
        "\t(label \"N3\" (at 50 80.5 0) (uuid \"l3\"))\n",
    ));
    assert_refused(
        "fix_connectivity",
        &path,
        json!({ "snap_tolerance": 1.0 }),
        "ambiguous_target",
        None,
    )
    .await;

    let (_dir, path) = fixture(&near_miss_source(
        "\t(label \"N1\" (at 40 80 0) (uuid \"l1\"))\n",
    ));
    assert_refused(
        "fix_connectivity",
        &path,
        json!({ "snap_tolerance": 1.0 }),
        "would_join_nets",
        None,
    )
    .await;

    // An unnamed wire between two labels: neither snap alone names two nets,
    // both together join N1 to N2.
    let (_dir, path) = fixture(&near_miss_source(
        "\t(label \"N1\" (at 39.5 80 0) (uuid \"l1\"))\n",
    ));
    assert_refused(
        "fix_connectivity",
        &path,
        json!({ "snap_tolerance": 1.0 }),
        "would_join_nets",
        None,
    )
    .await;

    let (_dir, path) = fixture(&near_miss_source(""));
    let fixed = call("fix_connectivity", &path, json!({ "snap_tolerance": 1.0 })).await;
    assert!(!fixed.is_error, "{fixed:?}");
    assert_eq!(body(&fixed)["fixes_found"], 1);
}

#[tokio::test]
async fn apply_template_refuses_an_unloadable_position() {
    let (_dir, path) = fixture(&sheet_source());
    for field in ["position_x", "position_y"] {
        let mut args = json!({ "template_id": "ldo_3v3" });
        args[field] = json!(1e9);
        assert_refused(
            "apply_template",
            &path,
            args,
            "invalid_argument",
            Some(field),
        )
        .await;
    }
}

#[tokio::test]
async fn bulk_move_reports_a_move_that_snaps_to_nothing_as_unchanged() {
    let (_dir, path) = fixture(&sheet_source());
    let before = read(&path);
    let result = call(
        "bulk_move_schematic_components",
        &path,
        json!({ "references": ["R1"], "dx": 0.5, "dy": 0.3 }),
    )
    .await;
    assert!(!result.is_error, "{result:?}");
    let body = body(&result);
    assert_eq!(body["moved_count"], 0, "{body}");
    assert_eq!(body["unchanged"], json!(["R1"]), "{body}");
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn justify_refusal_does_not_suggest_the_token_it_refused() {
    let (_dir, path) = fixture(&sheet_source());
    let before = read(&path);
    let result = call(
        "add_schematic_text",
        &path,
        json!({ "text": "note", "x": 50.8, "y": 50.8, "justify": "center bottom" }),
    )
    .await;
    assert!(result.is_error, "{result:?}");
    let message = format!("{:?}", result.content);
    assert!(!message.contains("or center"), "{message}");
    assert!(message.contains("omit"), "{message}");
    assert_eq!(read(&path), before);
}
