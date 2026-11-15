use super::*;
use crate::router::ToolRouter;
use crate::tools::ServerConfig;
use std::sync::Arc;

fn ctx() -> ToolContext {
    ToolContext::new(
        ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        },
        Arc::new(ToolRouter::new()),
    )
}

const WIRE_A: &str = "\t(wire (pts (xy 101.6 101.6) (xy 127 101.6)) (stroke (width 0) (type default)) (uuid \"wire-a\"))\n";
const WIRE_B: &str = "\t(wire (pts (xy 110.49 101.6) (xy 110.49 111.76)) (stroke (width 0) (type default)) (uuid \"wire-b\"))\n";
const JUNCTION: &str =
    "\t(junction (at 110.49 101.6) (diameter 0) (color 0 0 0 0) (uuid \"junc-1\"))\n";
const LABELS: &str = concat!(
    "\t(label \"NET1\" (at 110 90 0) (effects (font (size 1.27 1.27)) (justify left bottom)) (uuid \"l1\"))\n",
    "\t(global_label \"GLOB\" (shape input) (at 120 90 0) (effects (font (size 1.27 1.27))) (uuid \"l2\"))\n",
    "\t(hierarchical_label \"HIER\" (shape input) (at 130 90 0) (uuid \"l3\"))\n",
    "\t(label \"DUP\" (at 140 90 0) (uuid \"d1\"))\n",
    "\t(label \"DUP\" (at 140 90 0) (uuid \"d2\"))\n",
    "\t(label \"DUP\" (at 150 90 0) (uuid \"d3\"))\n",
);

fn sheet(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.kicad_sch");
    std::fs::write(
        &path,
        format!(
            "(kicad_sch\n\t(version 20260306)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n\t(lib_symbols)\n{body}\t(sheet_instances (path \"/\" (page \"1\")))\n)\n"
        ),
    )
    .unwrap();
    (dir, path)
}

fn wired() -> (tempfile::TempDir, std::path::PathBuf) {
    sheet(&format!("{WIRE_A}{WIRE_B}{JUNCTION}"))
}

fn text(result: &CallToolResult) -> String {
    format!("{:?}", result.content)
}

fn p(path: &std::path::Path) -> String {
    path.display().to_string()
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn delete_wire_by_uuid_removes_it_and_its_orphaned_junction() {
    let (_d, path) = wired();
    let r = handle_delete_wire(&json!({"schematic": p(&path), "uuid": "wire-b"}), &ctx())
        .await
        .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    assert!(text(&r).contains("orphaned junction"), "{}", text(&r));
    let after = read(&path);
    assert!(!after.contains("wire-b"));
    assert!(after.contains("wire-a"));
    assert!(
        !after.contains("junc-1"),
        "orphan junction must go: {after}"
    );
}

#[tokio::test]
async fn delete_wire_with_unknown_uuid_errors_and_leaves_file_untouched() {
    let (_d, path) = wired();
    let before = read(&path);
    let r = handle_delete_wire(&json!({"schematic": p(&path), "uuid": "nope"}), &ctx())
        .await
        .unwrap();
    assert!(r.is_error);
    assert!(text(&r).contains("'nope' not found"), "{}", text(&r));
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn delete_wire_matches_endpoints_in_either_direction() {
    let (_d, path) = wired();
    let r = handle_delete_wire(
        &json!({"schematic": p(&path), "x1": 127.0, "y1": 101.6, "x2": 101.6, "y2": 101.6}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    assert!(!read(&path).contains("wire-a"));
}

#[tokio::test]
async fn delete_wire_needs_uuid_or_all_four_coordinates() {
    let (_d, path) = wired();
    let before = read(&path);
    let r = handle_delete_wire(
        &json!({"schematic": p(&path), "x1": 101.6, "y1": 101.6}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(r.is_error);
    assert!(text(&r).contains("Provide either uuid"), "{}", text(&r));
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn delete_wire_with_no_matching_endpoints_errors_without_writing() {
    let (_d, path) = wired();
    let before = read(&path);
    let r = handle_delete_wire(
        &json!({"schematic": p(&path), "x1": 1.0, "y1": 1.0, "x2": 2.0, "y2": 1.0}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(r.is_error);
    assert!(text(&r).contains("Cannot locate a wire"), "{}", text(&r));
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn batch_delete_reports_unknown_uuids_but_deletes_the_known_ones() {
    let (_d, path) = wired();
    let r = handle_batch_delete_wire(
        &json!({"schematic": p(&path), "uuids": ["wire-b", "ghost"]}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    let t = text(&r);
    assert!(t.contains("ghost"), "unknown uuid must be reported: {t}");
    assert!(t.contains("deleted"), "{t}");
    let after = read(&path);
    assert!(!after.contains("wire-b") && after.contains("wire-a"));
}

#[tokio::test]
async fn batch_delete_with_only_unknown_uuids_errors_and_writes_nothing() {
    let (_d, path) = wired();
    let before = read(&path);
    let r = handle_batch_delete_wire(&json!({"schematic": p(&path), "uuids": ["ghost"]}), &ctx())
        .await
        .unwrap();
    assert!(r.is_error);
    assert!(text(&r).contains("No wires deleted"), "{}", text(&r));
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn batch_delete_with_an_empty_list_deletes_nothing_and_succeeds() {
    let (_d, path) = wired();
    let before = read(&path);
    let r = handle_batch_delete_wire(&json!({"schematic": p(&path), "uuids": []}), &ctx())
        .await
        .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn split_wire_replaces_it_with_two_halves_and_a_junction() {
    let (_d, path) = sheet(WIRE_A);
    let r = handle_split_wire_at_point(
        &json!({"schematic": p(&path), "x": 114.3, "y": 101.6}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    let tree = parse_sexp(&read(&path)).unwrap();
    let wires = extract_wires(&tree);
    assert_eq!(wires.len(), 2);
    assert!(wires
        .iter()
        .any(|w| (w.x1 - 101.6).abs() < 1e-6 && (w.x2 - 114.3).abs() < 1e-6));
    assert!(wires
        .iter()
        .any(|w| (w.x1 - 114.3).abs() < 1e-6 && (w.x2 - 127.0).abs() < 1e-6));
    assert_eq!(konnect_sexp::schematic::extract_junctions(&tree).len(), 1);
}

#[tokio::test]
async fn split_wire_at_an_endpoint_or_off_wire_is_refused_without_writing() {
    let (_d, path) = sheet(WIRE_A);
    let before = read(&path);
    for (x, y) in [(101.6, 101.6), (114.3, 50.0)] {
        let r = handle_split_wire_at_point(&json!({"schematic": p(&path), "x": x, "y": y}), &ctx())
            .await
            .unwrap();
        assert!(r.is_error, "({x},{y}) must be refused");
        assert!(text(&r).contains("No wire found"), "{}", text(&r));
    }
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn split_wire_requires_both_coordinates() {
    let (_d, path) = sheet(WIRE_A);
    for args in [json!({"x": 114.3}), json!({"y": 101.6})] {
        let mut full = args.clone();
        full["schematic"] = json!(p(&path));
        let r = handle_split_wire_at_point(&full, &ctx()).await.unwrap();
        assert!(r.is_error, "{}", text(&r));
    }
}

#[tokio::test]
async fn add_net_label_writes_each_label_type() {
    let (_d, path) = sheet("");
    for (ty, net) in [
        ("net_label", "N"),
        ("global_label", "G"),
        ("hierarchical_label", "H"),
    ] {
        let r = handle_add_net_label(
            &json!({"schematic": p(&path), "net": net, "x": 10.0, "y": 20.0,
                    "rotation": 180.0, "label_type": ty}),
            &ctx(),
        )
        .await
        .unwrap();
        assert!(!r.is_error, "{}", text(&r));
    }
    let after = read(&path);
    assert!(after.contains("(label \"N\""), "{after}");
    assert!(after.contains("(global_label \"G\""), "{after}");
    assert!(after.contains("(hierarchical_label \"H\""), "{after}");
    assert_eq!(after.matches("(justify right").count(), 3, "{after}");
}

#[tokio::test]
async fn add_net_label_refuses_a_missing_argument() {
    let (_d, path) = sheet("");
    let before = read(&path);
    for args in [
        json!({"net": "N", "y": 1.0}),
        json!({"net": "N", "x": 1.0}),
        json!({"x": 1.0, "y": 1.0}),
    ] {
        let mut full = args.clone();
        full["schematic"] = json!(p(&path));
        let r = handle_add_net_label(&full, &ctx()).await.unwrap();
        assert!(r.is_error, "{}", text(&r));
    }
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn delete_label_removes_exactly_the_one_at_the_position() {
    let (_d, path) = sheet(LABELS);
    let r = handle_delete_net_label(
        &json!({"schematic": p(&path), "net": "DUP", "x": 150.0, "y": 90.0}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    let after = read(&path);
    assert!(!after.contains("\"d3\"") && after.contains("\"d1\"") && after.contains("\"d2\""));
}

#[tokio::test]
async fn delete_label_refusals_name_the_candidates_and_write_nothing() {
    let (_d, path) = sheet(LABELS);
    let before = read(&path);
    let cases = [
        (
            json!({"net": "ABSENT", "x": 1.0, "y": 1.0}),
            "No label named 'ABSENT'",
        ),
        (
            json!({"net": "NET1", "x": 1.0, "y": 1.0}),
            "Found 1 label(s) named 'NET1'",
        ),
        (
            json!({"net": "DUP", "x": 140.0, "y": 90.0}),
            "2 labels named 'DUP' share position",
        ),
    ];
    for (args, expected) in cases {
        let mut full = args.clone();
        full["schematic"] = json!(p(&path));
        let r = handle_delete_net_label(&full, &ctx()).await.unwrap();
        assert!(r.is_error);
        assert!(text(&r).contains(expected), "{expected}: {}", text(&r));
    }
    for missing in ["net", "x", "y"] {
        let mut full = json!({"schematic": p(&path), "net": "NET1", "x": 110.0, "y": 90.0});
        full.as_object_mut().unwrap().remove(missing);
        let r = handle_delete_net_label(&full, &ctx()).await.unwrap();
        assert!(r.is_error, "missing {missing}");
    }
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn rotate_label_rewrites_anchor_and_justify_for_every_label_shape() {
    let (_d, path) = sheet(LABELS);
    for (net, x, expect) in [
        ("NET1", 110.0, "(justify right bottom)"),
        ("GLOB", 120.0, "(justify right)"),
        ("HIER", 130.0, "(justify right)"),
    ] {
        let r = handle_rotate_label(
            &json!({"schematic": p(&path), "net": net, "x": x, "y": 90.0, "rotation": 180.0}),
            &ctx(),
        )
        .await
        .unwrap();
        assert!(!r.is_error, "{net}: {}", text(&r));
        let after = read(&path);
        assert!(after.contains(expect), "{net}: {after}");
        assert!(
            after.contains(&format!("(at {x} 90 180)")),
            "{net}: {after}"
        );
    }
}

#[tokio::test]
async fn rotate_label_refusals_write_nothing() {
    let (_d, path) = sheet(LABELS);
    let before = read(&path);
    let r = handle_rotate_label(
        &json!({"schematic": p(&path), "net": "ABSENT", "x": 1.0, "y": 1.0, "rotation": 90.0}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(
        r.is_error && text(&r).contains("No label named 'ABSENT'"),
        "{}",
        text(&r)
    );
    let r = handle_rotate_label(
        &json!({"schematic": p(&path), "net": "NET1", "x": 1.0, "y": 1.0, "rotation": 90.0}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(
        r.is_error && text(&r).contains("No label 'NET1' at (1, 1). Found:"),
        "{}",
        text(&r)
    );
    for missing in ["net", "x", "y", "rotation"] {
        let mut full =
            json!({"schematic": p(&path), "net": "NET1", "x": 110.0, "y": 90.0, "rotation": 90.0});
        full.as_object_mut().unwrap().remove(missing);
        let r = handle_rotate_label(&full, &ctx()).await.unwrap();
        assert!(r.is_error, "missing {missing}");
    }
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn move_labels_shifts_every_same_named_label_and_keeps_rotation() {
    let (_d, path) = sheet(LABELS);
    let r = handle_move_labels_by_offset(
        &json!({"schematic": p(&path), "net": "DUP", "dx": 1.0, "dy": -2.0}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    let after = read(&path);
    assert_eq!(after.matches("(at 141 88 0)").count(), 2, "{after}");
    assert!(after.contains("(at 151 88 0)"), "{after}");
    assert!(after.contains("(at 110 90 0)"), "other nets must not move");
}

#[tokio::test]
async fn move_labels_refusals_write_nothing() {
    let (_d, path) = sheet(LABELS);
    let before = read(&path);
    let base = json!({"schematic": p(&path), "net": "NET1", "dx": 1.0, "dy": 1.0});
    let mut absent = base.clone();
    absent["net"] = json!("ABSENT");
    let r = handle_move_labels_by_offset(&absent, &ctx()).await.unwrap();
    assert!(r.is_error && text(&r).contains("No label named 'ABSENT'"));
    let mut huge = base.clone();
    huge["dx"] = json!(1e300);
    let r = handle_move_labels_by_offset(&huge, &ctx()).await.unwrap();
    assert!(r.is_error, "{}", text(&r));
    for missing in ["net", "dx", "dy"] {
        let mut full = base.clone();
        full.as_object_mut().unwrap().remove(missing);
        let r = handle_move_labels_by_offset(&full, &ctx()).await.unwrap();
        assert!(r.is_error, "missing {missing}");
    }
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn batch_rotate_labels_rotates_each_requested_label() {
    let (_d, path) = sheet(LABELS);
    let r = handle_batch_rotate_labels(
        &json!({"schematic": p(&path), "labels": [
            {"net": "NET1", "x": 110.0, "y": 90.0, "rotation": 90.0},
            {"net": "GLOB", "x": 120.0, "y": 90.0, "rotation": 270.0}
        ]}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    let after = read(&path);
    assert!(after.contains("(at 110 90 90)") && after.contains("(at 120 90 270)"));
}

#[tokio::test]
async fn batch_rotate_labels_does_not_report_a_missing_label_as_rotated() {
    let (_d, path) = sheet(LABELS);
    let r = handle_batch_rotate_labels(
        &json!({"schematic": p(&path), "labels": [
            {"net": "ABSENT", "x": 1.0, "y": 1.0, "rotation": 90.0}
        ]}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(r.is_error, "nothing was rotated, yet: {}", text(&r));
    assert!(text(&r).contains("ABSENT"), "{}", text(&r));
}
