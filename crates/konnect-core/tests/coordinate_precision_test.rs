use konnect_core::mcp::handler::McpHandler;
use konnect_core::tools::ServerConfig;
use konnect_sexp::{parse_sexp, SexpNode};
use serde_json::{json, Value};

fn assert_coordinate_precision(node: &SexpNode) {
    let children = node.children().unwrap_or_default();
    if matches!(
        node.head(),
        Some("at" | "xy" | "start" | "end" | "mid" | "size")
    ) {
        for child in children.iter().skip(1) {
            if let SexpNode::Atom(text) = child {
                if let Some((_, decimal)) = text.split_once('.') {
                    assert!(
                        decimal.len() <= 6,
                        "coordinate contains float noise: {text}"
                    );
                }
                assert_ne!(text, "-0");
            }
        }
    }
    for child in children {
        assert_coordinate_precision(child);
    }
}

async fn call(handler: &McpHandler, name: &str, arguments: Value) -> Value {
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .await
        .unwrap();
    assert!(response.error.is_none(), "{response:?}");
    let result = response.result.unwrap();
    assert_ne!(result["isError"], true, "{result}");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn served_label_and_text_writers_use_nanometre_precision() {
    let directory = tempfile::tempdir().unwrap();
    let schematic = directory.path().join("precision.kicad_sch");
    let board = directory.path().join("precision.kicad_pcb");
    let source = include_str!("fixtures/no_connect_carry_kicad10.kicad_sch").replace(
        "(label \"NETB\"\n\t\t(at 147.32 160.02 0)",
        "(label \"NETB\"\n\t\t(at 10 167.64 0)",
    );
    std::fs::write(&schematic, source).unwrap();
    std::fs::write(
        &board,
        include_str!("fixtures/specctra_two_resistors.kicad_pcb"),
    )
    .unwrap();
    let handler = McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();

    call(
        &handler,
        "move_labels_by_offset",
        json!({
            "schematic": schematic, "net": "NETB", "dx": 0.0, "dy": -5.08
        }),
    )
    .await;
    let saved = std::fs::read_to_string(&schematic).unwrap();
    assert!(saved.contains("(at 10 162.56 0)"), "{saved}");
    assert_coordinate_precision(&parse_sexp(&saved).unwrap());

    let noisy = 50.8 * 1.075;
    for (name, path_key, path) in [
        ("add_schematic_text", "schematic", &schematic),
        ("add_board_text", "board", &board),
    ] {
        let mut arguments =
            json!({"text": "1.234567890123456789 remains text", "x": noisy, "y": noisy});
        arguments[path_key] = json!(path);
        let body = call(&handler, name, arguments).await;
        assert_eq!(body["x"], 54.61, "{body}");
        assert_eq!(body["y"], 54.61, "{body}");
        let saved = std::fs::read_to_string(path).unwrap();
        assert!(saved.contains("(at 54.61 54.61 0)"), "{saved}");
        assert!(saved.contains("1.234567890123456789 remains text"));
        assert_coordinate_precision(&parse_sexp(&saved).unwrap());
    }

    for (name, path_key, path) in [
        ("add_schematic_text", "schematic", &schematic),
        ("add_board_text", "board", &board),
    ] {
        let original = std::fs::read(path).unwrap();
        let mut arguments = json!({"text": "invalid", "x": 1e300, "y": 0.0});
        arguments[path_key] = json!(path);
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            }))
            .await
            .unwrap();
        assert_eq!(response.result.unwrap()["isError"], true);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}

#[tokio::test]
async fn served_coordinate_review_overflow_refuses_without_file_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let schematic = directory.path().join("overflow.kicad_sch");
    let board = directory.path().join("overflow.kicad_pcb");
    std::fs::write(
        &schematic,
        include_str!("fixtures/no_connect_carry_kicad10.kicad_sch"),
    )
    .unwrap();
    std::fs::write(
        &board,
        include_str!("fixtures/specctra_two_resistors.kicad_pcb"),
    )
    .unwrap();
    let handler = McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    for (name, path, arguments) in [
        (
            "bulk_move_schematic_components",
            &schematic,
            json!({"schematic":schematic,"references":["R5"],"dx":1e308,"dy":0.0}),
        ),
        (
            "move_component",
            &board,
            json!({"board":board,"reference":"R1","x":1e308,"y":0.0}),
        ),
        (
            "add_schematic_text",
            &schematic,
            json!({"schematic":schematic,"text":"invalid font","x":10.0,"y":10.0,"thickness":1e308}),
        ),
    ] {
        let before = std::fs::read(path).unwrap();
        let response = handler.handle_message(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await.unwrap();
        let result = response.result.unwrap();
        assert_eq!(result["isError"], true, "{name}: {result}");
        assert_eq!(std::fs::read(path).unwrap(), before, "{name}");
    }
}
