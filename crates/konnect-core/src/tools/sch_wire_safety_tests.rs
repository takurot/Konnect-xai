use super::*;

#[test]
fn unrelated_wire_preserves_deliberate_undotted_tee() {
    let input = "(kicad_sch (wire (pts (xy 0 0) (xy 10 0))) (wire (pts (xy 5 0) (xy 5 5))))";
    let output = insert_wire_with_junctions(input.into(), 20., 20., 30., 20.).unwrap();
    let tree = parse_sexp(&output).unwrap();
    assert!(konnect_sexp::schematic::extract_junctions(&tree).is_empty());
}

#[test]
fn duplicate_reference_does_not_select_first_pin() {
    let tree = parse_sexp(
        r#"(kicad_sch
      (lib_symbols (symbol "Test:P" (symbol "P_1_1"
        (pin passive line (at 0 0 0) (length 1) (name "P") (number "1")))))
      (symbol (lib_id "Test:P") (at 0 0 0) (unit 1) (uuid "a") (property "Reference" "R1"))
      (symbol (lib_id "Test:P") (at 10 0 0) (unit 1) (uuid "b") (property "Reference" "R1")))"#,
    )
    .unwrap();
    let instances = extract_symbol_instances(&tree);
    let libs = tree.find("lib_symbols").unwrap().find_all("symbol");
    let error = resolve_pin_endpoint(&instances, &libs, "R1", "1").unwrap_err();
    assert!(error.to_string().contains("ambiguous_reference"));
}

fn context() -> ToolContext {
    ToolContext::new(
        Default::default(),
        std::sync::Arc::new(crate::router::ToolRouter::new()),
    )
}

fn route_fixture(duplicate: bool) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("route.kicad_sch");
    let mut source = String::from(
        r#"(kicad_sch (version 20260306) (uuid "root")
      (lib_symbols (symbol "Test:P" (symbol "P_1_1"
        (pin passive line (at 0 0 0) (length 1) (name "P") (number "1")))))"#,
    );
    for (reference, uuid, x, y) in [
        ("R1", "one", 0, 0),
        ("R2", "two", 10, 10),
        (if duplicate { "R1" } else { "C1" }, "foreign", 5, 0),
    ] {
        source.push_str(&format!(r#"(symbol (lib_id "Test:P") (at {x} {y} 0) (unit 1) (uuid "{uuid}") (property "Reference" "{reference}"))"#));
    }
    source.push_str(r#"(label "FOREIGN" (at 5 0 0)))"#);
    std::fs::write(&path, &source).unwrap();
    (dir, path, source)
}

#[tokio::test]
async fn single_pin_route_refuses_foreign_net_without_writing() {
    let (_dir, path, before) = route_fixture(false);
    let result = handle_connect_pins(
        &json!({"schematic":path,"ref1":"R1","pin1":"1","ref2":"R2","pin2":"1"}),
        &context(),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    let encoded = serde_json::to_string(&result).unwrap();
    assert!(
        encoded.contains("would_join_nets") && encoded.contains("FOREIGN"),
        "{encoded}"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[tokio::test]
async fn duplicate_reference_has_structured_candidates_and_preserves_file() {
    let (_dir, path, before) = route_fixture(true);
    let result = handle_connect_pins(
        &json!({"schematic":path,"ref1":"R1","pin1":"1","ref2":"R2","pin2":"1"}),
        &context(),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["error"]["kind"], "ambiguous_reference");
    assert_eq!(body["error"]["uuids"], json!(["one", "foreign"]));
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[test]
fn malformed_wire_source_is_not_silently_edited() {
    assert!(insert_wire_with_junctions("(kicad_sch (broken".into(), 0., 0., 10., 0.).is_err());
}

#[tokio::test]
async fn uuid_selectors_disambiguate_references() {
    let (_dir, path, _) = route_fixture(true);
    let result = handle_connect_pins(&json!({"schematic":path,"uuid1":"one","pin1":"1","uuid2":"two","pin2":"1","allow_join":true}),&context()).await.unwrap();
    assert!(!result.is_error, "{result:?}");
    let tree = parse_sexp(&std::fs::read_to_string(path).unwrap()).unwrap();
    let wires = extract_wires(&tree);
    assert_eq!(wires.len(), 2);
    assert!(wires.iter().any(|w| w.x1 == 0. && w.y1 == 0.));
}
