use super::*;
use crate::tools::ToolContext;
use std::path::{Path, PathBuf};

fn context() -> ToolContext {
    ToolContext::new(
        Default::default(),
        std::sync::Arc::new(crate::router::ToolRouter::new()),
    )
}

fn fixture(points: &[(&str, f64, f64)]) -> (tempfile::TempDir, PathBuf, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("route.kicad_sch");
    let mut content = String::from(
        r#"(kicad_sch (version 20250610) (uuid "11111111-1111-4111-8111-111111111111")
      (lib_symbols (symbol "Test:P" (symbol "P_1_1"
        (pin passive line (at 0 0 0) (length 0) (name "P") (number "1")))))"#,
    );
    for (index, &(reference, x, y)) in points.iter().enumerate() {
        content.push_str(&format!(
            r#"(symbol (lib_id "Test:P") (at {x} {y} 0) (unit 1)
          (uuid "22222222-2222-4222-8222-{index:012}")
          (property "Reference" "{reference}" (at {x} {y} 0)))"#
        ));
    }
    content.push(')');
    std::fs::write(&path, &content).unwrap();
    (directory, path, content)
}

fn assert_refused_unchanged(path: &Path, before: &str, result: CallToolResult) {
    assert!(result.is_error, "{result:?}");
    assert!(
        error_text(&result).contains("would_join_nets"),
        "{result:?}"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[tokio::test]
async fn later_crossing_route_refuses_the_whole_batch_without_writing() {
    let (_directory, path, before) = fixture(&[
        ("R1", 0., 0.),
        ("R2", 10., 0.),
        ("R3", 5., -5.),
        ("R4", 5., 5.),
    ]);
    let result = handle_batch_connect_pins(
        &json!({"schematic":path,"connections":[
          {"ref1":"R1","pin1":"1","ref2":"R2","pin2":"1"},
          {"ref1":"R3","pin1":"1","ref2":"R4","pin2":"1"}
        ]}),
        &context(),
    )
    .await
    .unwrap();
    assert_refused_unchanged(&path, &before, result);
}

#[tokio::test]
async fn net_stub_crossing_a_foreign_pin_refuses_without_writing() {
    let (_directory, path, before) = fixture(&[("R1", 0., 0.), ("C1", 5., 0.)]);
    let result = handle_batch_connect_to_net(&json!({"schematic":path,"net":"Signal","stub_length":10.,"direction":"right","pins":[{"reference":"R1","pin_number":"1"}]}), &context()).await.unwrap();
    assert_refused_unchanged(&path, &before, result);
}

#[tokio::test]
async fn allow_join_is_explicit_and_boolean() {
    let (_directory, path, before) = fixture(&[("R1", 0., 0.), ("C1", 5., 0.)]);
    let mut args = json!({"schematic":path,"net":"Signal","stub_length":10.,"direction":"right","pins":[{"reference":"R1","pin_number":"1"}],"allow_join":"yes"});
    let result = handle_batch_connect_to_net(&args, &context())
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    args["allow_join"] = json!(true);
    let result = handle_batch_connect_to_net(&args, &context())
        .await
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert_ne!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn passthrough_refuses_a_foreign_pin_without_writing() {
    let (_directory, path, before) = fixture(&[("R1", 0., 0.), ("C1", 1.27, 0.)]);
    let result = handle_connect_passthrough(
        &json!({"schematic":path,"net_name":"SIGNAL","x":0.,"y":0.,"direction":"right"}),
        &context(),
    )
    .await
    .unwrap();
    assert_refused_unchanged(&path, &before, result);
}

#[tokio::test]
async fn passthrough_writes_an_unobstructed_stub() {
    let (_directory, path, _before) = fixture(&[("R1", 0., 0.)]);
    let result = handle_connect_passthrough(
        &json!({"schematic":path,"net_name":"SIGNAL","x":0.,"y":0.,"direction":"right"}),
        &context(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    let text = std::fs::read_to_string(path).unwrap();
    let root = konnect_sexp::parse_sexp(&text).unwrap();
    let wire = konnect_sexp::schematic::extract_wires(&root).remove(0);
    assert_eq!((wire.x1, wire.y1, wire.x2, wire.y2), (0., 0., 2.54, 0.));
}
