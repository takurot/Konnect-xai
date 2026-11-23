use super::*;

#[tokio::test]
async fn batch_connect_pins_routes_vertical_first_when_horizontal_first_would_join_nets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("route.kicad_sch");
    let mut source = String::from(
        r#"(kicad_sch (version 20260306) (uuid "root")
      (lib_symbols (symbol "Test:P" (symbol "P_1_1"
        (pin passive line (at 0 0 0) (length 1) (name "P") (number "1")))))"#,
    );
    for (reference, x, y) in [("R1", 0, 0), ("R2", 10, 10), ("C1", 5, 0)] {
        source.push_str(&format!(
            r#"(symbol (lib_id "Test:P") (at {x} {y} 0) (unit 1) (uuid "{reference}") (property "Reference" "{reference}"))"#
        ));
    }
    source.push_str(r#"(label "FOREIGN" (at 5 0 0)))"#);
    std::fs::write(&path, &source).unwrap();

    let result = handle_batch_connect_pins(
        &json!({
            "schematic": path,
            "connections": [{ "ref1": "R1", "pin1": "1", "ref2": "R2", "pin2": "1" }]
        }),
        &crate::tools::ToolContext::new(
            Default::default(),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        ),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");

    let tree = konnect_sexp::parse_sexp(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut wires: Vec<_> = konnect_sexp::schematic::extract_wires(&tree)
        .iter()
        .map(|w| [w.x1, w.y1, w.x2, w.y2])
        .collect();
    wires.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(wires, [[0., 0., 0., 10.], [0., 10., 10., 10.]]);
}
