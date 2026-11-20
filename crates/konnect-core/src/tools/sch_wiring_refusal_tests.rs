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

fn body(result: &CallToolResult) -> serde_json::Value {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => {
            serde_json::from_str(text).unwrap()
        }
        other => panic!("expected text, got {other:?}"),
    }
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

const NO_CONNECTS: &str = concat!(
    "\t(no_connect\n\t\t(at 20 30)\n\t\t(uuid \"nc1\")\n\t)\n",
    "\t(no_connect\n\t\t(at 40 30)\n\t\t(uuid \"nc2\")\n\t)\n",
);

#[tokio::test]
async fn delete_no_connect_removes_the_one_at_the_position_or_refuses() {
    let (_d, path) = sheet(NO_CONNECTS);
    let before = read(&path);
    let r = handle_delete_no_connect(&json!({"schematic": p(&path), "x": 1.0, "y": 1.0}), &ctx())
        .await
        .unwrap();
    assert!(
        r.is_error && text(&r).contains("No-connect not found"),
        "{}",
        text(&r)
    );
    assert_eq!(read(&path), before);

    let r = handle_delete_no_connect(
        &json!({"schematic": p(&path), "x": 20.0, "y": 30.0}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    let after = read(&path);
    assert!(!after.contains("nc1") && after.contains("nc2"), "{after}");

    for missing in ["x", "y"] {
        let mut args = json!({"schematic": p(&path), "x": 40.0, "y": 30.0});
        args.as_object_mut().unwrap().remove(missing);
        let r = handle_delete_no_connect(&args, &ctx()).await.unwrap();
        assert!(r.is_error, "missing {missing}");
    }
}

#[tokio::test]
async fn batch_delete_no_connect_reports_each_miss_and_deletes_the_rest() {
    let (_d, path) = sheet(NO_CONNECTS);
    let r = handle_batch_delete_no_connect(
        &json!({"schematic": p(&path), "positions": [
            {"x": 20.0, "y": 30.0}, {"x": 20.0, "y": 30.0}, {"x": 9.0, "y": 9.0}, {"x": "bad"}
        ]}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    let body = body(&r);
    assert_eq!(body["deleted"], 1, "a repeated position deletes once");
    assert_eq!(body["errors"].as_array().unwrap().len(), 2, "{body}");
    let after = read(&path);
    assert!(!after.contains("nc1") && after.contains("nc2"), "{after}");
}

#[tokio::test]
async fn batch_delete_no_connect_refuses_when_nothing_matches_and_allows_an_empty_list() {
    let (_d, path) = sheet(NO_CONNECTS);
    let before = read(&path);
    let r = handle_batch_delete_no_connect(
        &json!({"schematic": p(&path), "positions": [{"x": 9.0, "y": 9.0}]}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(
        r.is_error && text(&r).contains("No no-connects deleted"),
        "{}",
        text(&r)
    );
    assert_eq!(read(&path), before);

    let r =
        handle_batch_delete_no_connect(&json!({"schematic": p(&path), "positions": []}), &ctx())
            .await
            .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn junctions_are_added_singly_and_in_batches_and_need_both_coordinates() {
    let (_d, path) = sheet("");
    let r = handle_add_junction(
        &json!({"schematic": p(&path), "x": 10.0, "y": 20.0}),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!r.is_error, "{}", text(&r));
    let r = handle_batch_add_junction(
        &json!({"schematic": p(&path), "positions": [{"x": 1.0, "y": 2.0}, {"x": 3.0, "y": 4.0}]}),
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(body(&r)["added"], 2);
    let tree = parse_sexp(&read(&path)).unwrap();
    assert_eq!(konnect_sexp::schematic::extract_junctions(&tree).len(), 3);

    let before = read(&path);
    for missing in ["x", "y"] {
        let mut args = json!({"schematic": p(&path), "x": 1.0, "y": 1.0});
        args.as_object_mut().unwrap().remove(missing);
        assert!(handle_add_junction(&args, &ctx()).await.unwrap().is_error);
    }
    assert!(
        handle_batch_add_junction(&json!({"schematic": p(&path)}), &ctx())
            .await
            .unwrap()
            .is_error
    );
    assert_eq!(read(&path), before);
}

const FOREIGN_NET: &str = concat!(
    "\t(wire (pts (xy 2.54 -5.08) (xy 2.54 5.08)) (stroke (width 0) (type default)) (uuid \"w-foreign\"))\n",
    "\t(label \"FOREIGN\" (at 2.54 0 0) (uuid \"l-foreign\"))\n",
);

#[tokio::test]
async fn adding_a_wire_across_another_net_is_refused_unless_a_join_is_allowed() {
    let (_d, path) = sheet(FOREIGN_NET);
    let before = read(&path);
    let wire = json!({"schematic": p(&path), "x1": 0.0, "y1": 0.0, "x2": 5.08, "y2": 0.0});
    let r = handle_add_wire(&wire, &ctx()).await.unwrap();
    assert!(r.is_error, "{}", text(&r));
    assert!(text(&r).contains("FOREIGN"), "{}", text(&r));
    assert_eq!(read(&path), before, "a refused wire must not be written");

    let batch = json!({"schematic": p(&path), "wires": [
        {"x1": 0.0, "y1": 0.0, "x2": 5.08, "y2": 0.0}
    ]});
    let r = handle_batch_add_wire(&batch, &ctx()).await.unwrap();
    assert!(r.is_error, "{}", text(&r));
    assert_eq!(read(&path), before);

    let mut allowed = wire.clone();
    allowed["allow_join"] = json!(true);
    let r = handle_add_wire(&allowed, &ctx()).await.unwrap();
    assert!(!r.is_error, "{}", text(&r));
    assert_ne!(read(&path), before);
}

#[tokio::test]
async fn connect_to_net_refuses_an_incomplete_pin_and_a_route_onto_another_net() {
    let (_d, path) = sheet(FOREIGN_NET);
    let before = read(&path);
    let base = json!({"schematic": p(&path), "net": "SIG"});
    let with = |extra: serde_json::Value| {
        let mut args = base.clone();
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        args
    };
    for (args, needle) in [
        (with(json!({"reference": "R1"})), "must be given together"),
        (with(json!({"pin_number": "1"})), "must be given together"),
        (with(json!({"pin_y": 0.0})), "Missing 'pin_x'"),
        (with(json!({"pin_x": 5.08})), "Missing 'pin_y'"),
        (
            with(json!({"pin_x": 0.0, "pin_y": 0.0, "direction": "right"})),
            "FOREIGN",
        ),
    ] {
        let r = handle_connect_to_net(&args, &ctx()).await.unwrap();
        assert!(r.is_error, "{args}: {}", text(&r));
        assert!(text(&r).contains(needle), "{needle}: {}", text(&r));
    }
    assert_eq!(read(&path), before);
}

#[tokio::test]
async fn connect_to_net_checks_the_endpoint_net_before_writing() {
    let (_dir, path) = sheet("");
    let before = read(&path).replace(
        "\t(lib_symbols)\n",
        r#"
  (lib_symbols (symbol "Fixture:P" (symbol "P_1_1"
    (pin passive line (at 0 0 0) (length 1) (name "P") (number "1")))))
  (wire (pts (xy 10 20) (xy 20 20)) (stroke (width 0) (type default)) (uuid "wire"))
  (label "EXISTING" (at 20 20 0) (uuid "local"))
  (global_label "WINNING" (shape input) (at 20 20 0) (uuid "global"))
  (symbol (lib_id "Fixture:P") (at 10 20 0) (unit 1) (uuid "22222222-2222-2222-2222-222222222222")
    (property "Reference" "R1") (property "Value" "P"))
"#,
    );
    std::fs::write(&path, &before).unwrap();
    for selector in [
        json!({"reference": "R1", "pin_number": "1"}),
        json!({"uuid": "22222222-2222-2222-2222-222222222222", "pin_number": "1"}),
        json!({"pin_x": 10.0, "pin_y": 20.0}),
    ] {
        let mut args = selector;
        args["schematic"] = json!(p(&path));
        args["net"] = json!("OTHER");
        args["direction"] = json!("left");
        let refusal = handle_connect_to_net(&args, &ctx()).await.unwrap();
        assert!(refusal.is_error, "unexpected success: {args}");
        let error = &body(&refusal)["error"];
        assert_eq!(error["kind"], "would_join_nets");
        assert_eq!(error["nets"], json!(["EXISTING", "OTHER", "WINNING"]));
        assert_eq!(error["at"], json!([[10.0, 20.0]]));
        assert_eq!(read(&path), before);

        // The requested alias is present even when another label wins naming.
        args["net"] = json!("EXISTING");
        args["allow_join"] = json!(true);
        let same = handle_connect_to_net(&args, &ctx()).await.unwrap();
        assert!(!same.is_error, "{}", text(&same));
        assert_eq!(
            body(&same),
            json!({"connected": "EXISTING", "already_connected": true})
        );
        assert_eq!(read(&path), before);
    }

    let allowed = handle_connect_to_net(
        &json!({
            "schematic": p(&path), "reference": "R1", "pin_number": "1",
            "net": "OTHER", "direction": "left", "allow_join": true
        }),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!allowed.is_error, "{}", text(&allowed));
    assert_eq!(body(&allowed)["connected"], "OTHER");
    assert_ne!(read(&path), before);
}

#[tokio::test]
async fn connect_to_net_repeating_a_stub_leaves_only_one_wire_and_label() {
    let (_dir, path) = sheet("");
    let args = json!({
        "schematic": p(&path), "pin_x": 10.0, "pin_y": 20.0,
        "net": "TARGET", "direction": "right"
    });
    let first = handle_connect_to_net(&args, &ctx()).await.unwrap();
    assert!(!first.is_error, "{}", text(&first));
    assert!(body(&first)["wire"].is_object());
    let connected = read(&path);
    let tree = parse_sexp(&connected).unwrap();
    assert_eq!(tree.find_all("wire").len(), 1);
    assert_eq!(tree.find_all("label").len(), 1);

    let second = handle_connect_to_net(&args, &ctx()).await.unwrap();
    assert!(!second.is_error, "{}", text(&second));
    assert_eq!(body(&second)["already_connected"], true);
    assert_eq!(read(&path), connected);
}

#[tokio::test]
async fn connect_to_net_does_not_conflate_same_text_label_scopes() {
    let (_dir, path) = sheet(concat!(
        "  (label \"SIG\" (at 10 20 0) (uuid \"local\"))\n",
        "  (global_label \"SIG\" (shape input) (at 31.27 30 0) (uuid \"global\"))\n",
    ));
    let before = read(&path);
    let mut at_local = json!({
        "schematic": p(&path), "pin_x": 10.0, "pin_y": 20.0,
        "net": "SIG", "label_type": "global_label", "direction": "left"
    });
    let at_unrelated_global = json!({
        "schematic": p(&path), "pin_x": 30.0, "pin_y": 30.0,
        "net": "SIG", "label_type": "net_label", "direction": "right"
    });
    for args in [&at_local, &at_unrelated_global] {
        let refusal = handle_connect_to_net(args, &ctx()).await.unwrap();
        assert!(refusal.is_error, "{}", text(&refusal));
        assert_eq!(body(&refusal)["error"]["kind"], "would_join_nets");
        assert_eq!(read(&path), before);
    }

    at_local["allow_join"] = json!(true);
    let joined = handle_connect_to_net(&at_local, &ctx()).await.unwrap();
    assert!(!joined.is_error, "{}", text(&joined));
    assert!(body(&joined)["wire"].is_object());
    let after = read(&path);
    assert_ne!(before, after);
    let parsed = parse_sexp(&after).unwrap();
    assert_eq!(parsed.find_all("global_label").len(), 2);

    let repeated = handle_connect_to_net(&at_local, &ctx()).await.unwrap();
    assert!(!repeated.is_error, "{}", text(&repeated));
    assert_eq!(body(&repeated)["already_connected"], true);
    assert_eq!(read(&path), after);
}
