//! #133: every schematic batch tool reports the same `outcome` block, refuses
//! an all-failed batch with `isError: true`, and leaves the file untouched
//! when nothing was applied.

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
         \t\t(property \"Footprint\" \"\" (at {x} {y} 0))\n\
         \t\t(instances (project \"s\" (path \"/root\" (reference \"{reference}\") (unit 1))))\n\
         \t)\n"
    )
}

/// R1's pins are at (101.6, 97.79) and (101.6, 105.41); R2's at (127, 97.79)
/// and (127, 105.41). Nothing is wired to them.
fn sheet_source() -> String {
    format!(
        "(kicad_sch\n\t(version 20250610)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n\t(paper \"A4\")\n\
         \t(lib_symbols\n\t\t(symbol \"Device:R\"\n\t\t\t(symbol \"R_1_1\"\n\
         \t\t\t\t(pin passive line (at 0 3.81 270) (length 1.27) (name \"~\") (number \"1\"))\n\
         \t\t\t\t(pin passive line (at 0 -3.81 90) (length 1.27) (name \"~\") (number \"2\"))\n\
         \t\t\t)\n\t\t)\n\t)\n\
         \t(wire (pts (xy 50.8 127) (xy 76.2 127)) (uuid \"w-free\"))\n\
         \t(label \"NET1\" (at 50.8 127 0) (effects (font (size 1.27 1.27)) (justify left bottom)) (uuid \"l1\"))\n\
         \t(no_connect\n\t\t(at 20.32 30.48)\n\t\t(uuid \"nc1\")\n\t)\n\
         {}{}\
         \t(sheet_instances (path \"/\" (page \"1\")))\n)\n",
        symbol("R1", "u-r1", 101.6, 101.6),
        symbol("R2", "u-r2", 127.0, 101.6),
    )
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.kicad_sch");
    std::fs::write(&path, sheet_source()).unwrap();
    (dir, path)
}

fn all_tools() -> Vec<ToolDef> {
    [
        sch_wiring::tools(),
        sch_components::tools(),
        sch_batch::tools(),
        sch_bus::tools(),
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

fn failure_indexes(outcome: &Value) -> Vec<u64> {
    outcome["failures"]
        .as_array()
        .unwrap_or_else(|| panic!("failures is a list: {outcome}"))
        .iter()
        .map(|failure| {
            assert!(
                failure["kind"]
                    .as_str()
                    .is_some_and(|kind| !kind.is_empty()),
                "{failure}"
            );
            assert!(failure["message"].is_string(), "{failure}");
            failure["index"].as_u64().expect("failure index")
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    PerItem,
    AllOrNothing,
}

struct Case {
    tool: &'static str,
    mode: Mode,
    all_fail: Value,
    /// Item 0 is valid, item 1 is not. `None` when the fixture cannot place a
    /// valid item (no library is configured for placement).
    mixed: Option<Value>,
}

fn cases() -> Vec<Case> {
    use Mode::*;
    vec![
        Case {
            tool: "connect_pins_to_bus",
            mode: PerItem,
            all_fail: json!({"bus_y": 81.28, "connections": [
                {"reference": "R99", "pin_number": "1", "net": "D0"}]}),
            mixed: Some(json!({"bus_y": 81.28, "connections": [
                {"reference": "R1", "pin_number": "1", "net": "D0"},
                {"reference": "R99", "pin_number": "1", "net": "D1"}]})),
        },
        Case {
            tool: "batch_connect_to_net",
            mode: PerItem,
            all_fail: json!({"net_name": "N", "pins": [{"reference": "R99", "pin_number": "1"}]}),
            mixed: Some(json!({"net_name": "N", "pins": [
                {"reference": "R1", "pin_number": "1"},
                {"reference": "R99", "pin_number": "1"}]})),
        },
        Case {
            tool: "batch_connect_pins",
            mode: PerItem,
            all_fail: json!({"connections": [
                {"ref1": "R99", "pin1": "1", "ref2": "R1", "pin2": "1"}]}),
            mixed: Some(json!({"connections": [
                {"ref1": "R1", "pin1": "2", "ref2": "R2", "pin2": "2"},
                {"ref1": "R99", "pin1": "1", "ref2": "R1", "pin2": "1"}]})),
        },
        Case {
            tool: "batch_edit_schematic_components",
            mode: PerItem,
            all_fail: json!({"edits": [{"reference": "R99", "value": "1k"}]}),
            mixed: Some(json!({"edits": [
                {"reference": "R1", "value": "1k"},
                {"reference": "R99", "value": "1k"}]})),
        },
        Case {
            tool: "bulk_move_schematic_components",
            mode: PerItem,
            all_fail: json!({"references": ["R99"], "dx": 2.54, "dy": 0.0}),
            mixed: Some(json!({"references": ["R1", "R99"], "dx": 2.54, "dy": 0.0})),
        },
        Case {
            tool: "batch_place_components",
            mode: PerItem,
            all_fail: json!({"components": [{"lib_id": "Device:R"}]}),
            mixed: None,
        },
        Case {
            tool: "batch_delete_schematic_components",
            mode: PerItem,
            all_fail: json!({"references": ["R99"]}),
            mixed: Some(json!({"references": ["R1", "R99"]})),
        },
        Case {
            tool: "batch_delete",
            mode: PerItem,
            all_fail: json!({"uuids": ["no-such-uuid"]}),
            mixed: Some(json!({"uuids": ["w-free", "no-such-uuid"]})),
        },
        Case {
            tool: "group_components",
            mode: PerItem,
            all_fail: json!({"references": ["R99"], "group_name": "G"}),
            mixed: Some(json!({"references": ["R1", "R99"], "group_name": "G"})),
        },
        Case {
            tool: "batch_rotate_labels",
            mode: AllOrNothing,
            all_fail: json!({"labels": [{"net": "NOPE", "x": 1.0, "y": 1.0, "rotation": 90.0}]}),
            mixed: Some(json!({"labels": [
                {"net": "NET1", "x": 50.8, "y": 127.0, "rotation": 90.0},
                {"net": "NOPE", "x": 1.0, "y": 1.0, "rotation": 90.0}]})),
        },
        Case {
            tool: "batch_add_wire",
            mode: AllOrNothing,
            all_fail: json!({"wires": [{"x1": 25.4, "y1": 25.4, "x2": 30.48}]}),
            mixed: Some(json!({"wires": [
                {"x1": 25.4, "y1": 25.4, "x2": 30.48, "y2": 25.4},
                {"x1": 25.4, "y1": 50.8, "x2": 30.48}]})),
        },
        Case {
            tool: "batch_delete_schematic_wire",
            mode: PerItem,
            all_fail: json!({"uuids": ["no-such-wire"]}),
            mixed: Some(json!({"uuids": ["w-free", "no-such-wire"]})),
        },
        Case {
            tool: "batch_delete_no_connect",
            mode: PerItem,
            all_fail: json!({"positions": [{"x": 1.0, "y": 1.0}]}),
            mixed: Some(json!({"positions": [{"x": 20.32, "y": 30.48}, {"x": 1.0, "y": 1.0}]})),
        },
    ]
}

fn assert_refused_untouched(tool: &str, result: &CallToolResult, before: &str, path: &Path) {
    let response = body(result);
    assert!(result.is_error, "{tool}: {response}");
    let kind = response["error"]["kind"].as_str().unwrap_or_default();
    assert!(
        !kind.is_empty() && kind != "handler_error",
        "{tool} needs a structured error kind: {response}"
    );
    let outcome = &response["outcome"];
    assert_eq!(outcome["status"], "failed", "{tool}: {response}");
    assert_eq!(outcome["applied"], json!([]), "{tool}: {response}");
    assert_eq!(outcome["written"], false, "{tool}: {response}");
    assert_eq!(read(path), before, "{tool} changed the file");
}

#[tokio::test]
async fn all_failed_batches_are_errors_and_leave_the_file_untouched() {
    for case in cases() {
        let (_dir, path) = fixture();
        let before = read(&path);
        let result = call(case.tool, &path, case.all_fail.clone()).await;
        assert_refused_untouched(case.tool, &result, &before, &path);
        let outcome = &body(&result)["outcome"];
        assert_eq!(
            failure_indexes(outcome),
            vec![0],
            "{}: {outcome}",
            case.tool
        );
        assert_eq!(outcome["requested"], 1, "{}: {outcome}", case.tool);
    }
}

#[tokio::test]
async fn one_bad_item_is_partial_or_refuses_the_batch_by_the_tool_contract() {
    for case in cases() {
        let Some(mixed) = case.mixed.clone() else {
            continue;
        };
        let (_dir, path) = fixture();
        let before = read(&path);
        let result = call(case.tool, &path, mixed).await;
        let response = body(&result);
        let outcome = &response["outcome"];
        assert_eq!(
            failure_indexes(outcome),
            vec![1],
            "{}: {response}",
            case.tool
        );
        assert_eq!(outcome["requested"], 2, "{}: {response}", case.tool);
        match case.mode {
            Mode::PerItem => {
                assert!(!result.is_error, "{}: {response}", case.tool);
                assert_eq!(outcome["status"], "partial", "{}: {response}", case.tool);
                assert_eq!(outcome["applied"], json!([0]), "{}: {response}", case.tool);
                assert_eq!(outcome["written"], true, "{}: {response}", case.tool);
                assert_eq!(
                    outcome["retry"]["item_indexes"],
                    json!([1]),
                    "{}: {response}",
                    case.tool
                );
                assert_ne!(read(&path), before, "{} wrote nothing", case.tool);
            }
            Mode::AllOrNothing => {
                assert_refused_untouched(case.tool, &result, &before, &path);
            }
        }
    }
}

#[tokio::test]
async fn every_listed_tool_states_its_write_contract_and_adopts_the_outcome() {
    for case in cases() {
        let definition = all_tools()
            .into_iter()
            .find(|definition| definition.name == case.tool)
            .unwrap();
        let description = definition.description.to_lowercase();
        let stated = match case.mode {
            Mode::PerItem => "per item",
            Mode::AllOrNothing => "all-or-nothing",
        };
        assert!(
            description.contains(stated),
            "{} must state it writes {stated}: {description}",
            case.tool
        );
        assert!(
            crate::outcome::ADOPTED_OUTCOME_TOOLS.contains(&case.tool),
            "{} is not in the adopted outcome catalogue",
            case.tool
        );
    }
}

#[tokio::test]
async fn edit_of_a_missing_component_names_the_component() {
    let (_dir, path) = fixture();
    let result = call(
        "batch_edit_schematic_components",
        &path,
        json!({"edits": [{"reference": "R99", "value": "1k", "footprint": "R:R_0603"}]}),
    )
    .await;
    let response = body(&result);
    let failures = response["outcome"]["failures"].as_array().unwrap().clone();
    assert_eq!(failures.len(), 1, "one failure per item: {response}");
    assert_eq!(failures[0]["kind"], "not_found", "{response}");
    assert_eq!(failures[0]["message"], "Component 'R99' not found");
}

/// batch_edit keeps its field-by-field contract: the valid field of an edit
/// with a refused field still lands, so that edit is both applied and failed.
#[tokio::test]
async fn edit_item_with_a_bad_field_is_applied_and_failed() {
    let (_dir, path) = fixture();
    let result = call(
        "batch_edit_schematic_components",
        &path,
        json!({"edits": [
            {"reference": "R2", "value": "2k2"},
            {"reference": "R1", "value": "4k7", "fields": {"Reference": "R5"}}
        ]}),
    )
    .await;
    let response = body(&result);
    let outcome = &response["outcome"];
    assert!(!result.is_error, "{response}");
    assert_eq!(outcome["status"], "partial", "{response}");
    assert_eq!(outcome["applied"], json!([0, 1]), "{response}");
    assert_eq!(failure_indexes(outcome), vec![1], "{response}");
    assert_eq!(outcome["retry"]["item_indexes"], json!([1]), "{response}");
    let after = read(&path);
    assert!(
        after.contains("\"2k2\"") && after.contains("\"4k7\""),
        "{after}"
    );
    assert!(!after.contains("\"R5\""), "{after}");
}

#[tokio::test]
async fn complete_batches_report_complete_and_written() {
    let (_dir, path) = fixture();
    let result = call(
        "batch_rotate_labels",
        &path,
        json!({"labels": [{"net": "NET1", "x": 50.8, "y": 127.0, "rotation": 90.0}]}),
    )
    .await;
    let outcome = &body(&result)["outcome"];
    assert!(!result.is_error, "{outcome}");
    assert_eq!(outcome["status"], "complete");
    assert_eq!(outcome["applied"], json!([0]));
    assert_eq!(outcome["failures"], json!([]));
    assert_eq!(outcome["written"], true);
    assert!(read(&path).contains("(at 50.8 127 90)"));
}

#[tokio::test]
async fn an_already_satisfied_batch_is_complete_without_a_write() {
    let (_dir, path) = fixture();
    let args = json!({"net_name": "N", "pins": [{"reference": "R1", "pin_number": "1"}]});
    call("batch_connect_to_net", &path, args.clone()).await;
    let before = read(&path);
    let result = call("batch_connect_to_net", &path, args).await;
    let outcome = &body(&result)["outcome"];
    assert_eq!(outcome["status"], "complete", "{outcome}");
    assert_eq!(outcome["applied"], json!([0]));
    assert_eq!(outcome["written"], false);
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn batch_delete_indexes_count_references_then_uuids() {
    let (_dir, path) = fixture();
    let result = call(
        "batch_delete",
        &path,
        json!({"references": ["R99", "R1"], "uuids": ["no-such-uuid", "w-free"]}),
    )
    .await;
    let response = body(&result);
    let outcome = &response["outcome"];
    assert!(!result.is_error, "{response}");
    assert_eq!(outcome["requested"], 4, "{response}");
    assert_eq!(outcome["applied"], json!([1, 3]), "{response}");
    assert_eq!(failure_indexes(outcome), vec![0, 2], "{response}");
    let after = read(&path);
    assert!(
        !after.contains("w-free") && !after.contains("u-r1"),
        "{after}"
    );
}

/// A vertical wire at x = 114.3 crosses the straight route from R1 pin 2 to
/// R2 pin 2 mid-span, so that route would join it and is refused.
async fn fixture_with_a_crossing_wire() -> (tempfile::TempDir, PathBuf) {
    let (dir, path) = fixture();
    let added = call(
        "batch_add_wire",
        &path,
        json!({"wires": [{"x1": 114.3, "y1": 100.33, "x2": 114.3, "y2": 110.49}]}),
    )
    .await;
    assert!(!added.is_error, "{}", body(&added));
    (dir, path)
}

#[tokio::test]
async fn a_whole_batch_refusal_keeps_its_kind_and_every_item_failure() {
    let (_dir, path) = fixture_with_a_crossing_wire().await;
    let before = read(&path);
    let result = call(
        "batch_connect_pins",
        &path,
        json!({"connections": [
            {"ref1": "R99", "pin1": "1", "ref2": "R1", "pin2": "1"},
            {"ref1": "R1", "pin1": "2", "ref2": "R2", "pin2": "2"}
        ]}),
    )
    .await;
    let response = body(&result);
    assert!(result.is_error, "{response}");
    assert_eq!(response["error"]["kind"], "would_join_nets", "{response}");
    let outcome = &response["outcome"];
    assert_eq!(failure_indexes(outcome), vec![0, 1], "{response}");
    assert_eq!(outcome["failures"][0]["kind"], "not_found", "{response}");
    assert_eq!(outcome["failures"][1]["kind"], "would_join_nets");
    assert_eq!(outcome["applied"], json!([]), "{response}");
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn a_refusal_after_an_item_was_planned_still_applies_nothing() {
    let (_dir, path) = fixture_with_a_crossing_wire().await;
    let before = read(&path);
    let result = call(
        "batch_connect_pins",
        &path,
        json!({"connections": [
            {"ref1": "R1", "pin1": "1", "ref2": "R2", "pin2": "1"},
            {"ref1": "R1", "pin1": "2", "ref2": "R2", "pin2": "2"}
        ]}),
    )
    .await;
    let response = body(&result);
    assert_eq!(response["error"]["kind"], "would_join_nets", "{response}");
    assert_eq!(response["outcome"]["applied"], json!([]), "{response}");
    assert_eq!(failure_indexes(&response["outcome"]), vec![1], "{response}");
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn all_or_nothing_wires_write_nothing_when_a_later_route_is_refused() {
    let (_dir, path) = fixture_with_a_crossing_wire().await;
    let before = read(&path);
    let result = call(
        "batch_add_wire",
        &path,
        json!({"wires": [
            {"x1": 25.4, "y1": 25.4, "x2": 30.48, "y2": 25.4},
            {"x1": 107.95, "y1": 105.41, "x2": 120.65, "y2": 105.41}
        ]}),
    )
    .await;
    let response = body(&result);
    assert!(result.is_error, "{response}");
    assert_eq!(response["error"]["kind"], "would_join_nets", "{response}");
    assert_eq!(failure_indexes(&response["outcome"]), vec![1], "{response}");
    assert_eq!(read(&path), before, "wire 0 must not be written");
}

#[tokio::test]
async fn every_repeated_item_is_reported_under_its_own_index() {
    let (_dir, path) = fixture();
    let result = call(
        "group_components",
        &path,
        json!({"references": ["R1", "R99", "R1", "R99"], "group_name": "G"}),
    )
    .await;
    let response = body(&result);
    let outcome = &response["outcome"];
    assert_eq!(outcome["applied"], json!([0, 2]), "{response}");
    assert_eq!(failure_indexes(outcome), vec![1, 3], "{response}");
}

#[tokio::test]
async fn rotating_one_label_twice_lands_the_last_angle_in_one_write() {
    let (_dir, path) = fixture();
    let result = call(
        "batch_rotate_labels",
        &path,
        json!({"labels": [
            {"net": "NET1", "x": 50.8, "y": 127.0, "rotation": 90.0},
            {"net": "NET1", "x": 50.8, "y": 127.0, "rotation": 180.0}
        ]}),
    )
    .await;
    assert_eq!(body(&result)["outcome"]["status"], "complete");
    assert!(read(&path).contains("(at 50.8 127 180)"));
}
