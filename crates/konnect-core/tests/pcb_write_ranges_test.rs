use konnect_core::mcp::handler::McpHandler;
use konnect_core::tools::ServerConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

struct Project {
    _dir: tempfile::TempDir,
    board: PathBuf,
    pro: PathBuf,
    handler: McpHandler,
}

async fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let board = dir.path().join("ranges.kicad_pcb");
    let pro = dir.path().join("ranges.kicad_pro");
    std::fs::write(
        &board,
        include_str!("fixtures/specctra_two_resistors.kicad_pcb"),
    )
    .unwrap();
    std::fs::write(
        &pro,
        r#"{"board":{"design_settings":{}},"meta":{"filename":"ranges.kicad_pro","version":3}}"#,
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
    Project {
        _dir: dir,
        board,
        pro,
        handler,
    }
}

async fn call(handler: &McpHandler, name: &str, arguments: Value) -> (bool, Value) {
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .await
        .unwrap();
    let result = response.result.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let body = serde_json::from_str(text).unwrap_or_else(|_| json!(text));
    (result["isError"] == true, body)
}

async fn assert_refused_unchanged(p: &Project, path: &Path, name: &str, args: Value, field: &str) {
    let before = std::fs::read(path).unwrap();
    let (is_error, body) = call(&p.handler, name, args.clone()).await;
    assert!(is_error, "{name} {args} should be refused: {body}");
    assert_eq!(
        body["error"]["kind"], "invalid_argument",
        "{name} {args}: {body}"
    );
    assert_eq!(body["error"]["field"], field, "{name} {args}: {body}");
    assert_eq!(std::fs::read(path).unwrap(), before, "{name} {args} wrote");
}

#[tokio::test]
async fn board_coordinates_outside_kicad_range_are_refused() {
    let p = project().await;
    let b = p.board.to_str().unwrap();
    let cases = [
        (
            "move_component",
            json!({"board": b, "reference": "R1", "x": 3000.0, "y": 115.0}),
            "x",
        ),
        (
            "move_component",
            json!({"board": b, "reference": "R1", "x": 10.0, "y": -2147.483648}),
            "y",
        ),
        (
            "add_board_text",
            json!({"board": b, "text": "T", "x": 1e12, "y": 0.0}),
            "x",
        ),
        (
            "add_via",
            json!({"board": b, "net_name": "GND", "x": 0.0, "y": 2200.0}),
            "y",
        ),
        (
            "route_trace",
            json!({"board": b, "net_name": "GND", "layer": "F.Cu", "width": 0.25,
                   "x1": 0.0, "y1": 0.0, "x2": 2148.0, "y2": 0.0}),
            "x2",
        ),
        (
            "add_board_outline",
            json!({"board": b, "x1": -2148.0, "y1": 0.0, "x2": 10.0, "y2": 10.0}),
            "x1",
        ),
        (
            "add_mounting_hole",
            json!({"board": b, "x": 0.0, "y": 3000.0}),
            "y",
        ),
        (
            "set_component_placements",
            json!({"board": b, "placements": [
                {"reference": "R1", "x": 3000.0, "y": 0.0, "rotation": 0.0}
            ]}),
            "placements[0].x",
        ),
        (
            "align_components",
            json!({"board": b, "references": ["R1"], "axis": "x", "value": 3000.0}),
            "value",
        ),
        (
            "duplicate_component",
            json!({"board": b, "reference": "R1", "new_reference": "R9", "x": 3000.0, "y": 0.0}),
            "x",
        ),
        (
            "place_component_array",
            json!({"board": b, "footprint": "Resistor_SMD:R_0402", "start_x": 2000.0,
                   "start_y": 0.0, "count_x": 3, "spacing_x": 100.0}),
            "spacing_x",
        ),
        (
            "route_differential_pair",
            json!({"board": b, "net_pos": "P", "net_neg": "N",
                   "x1": 2147.4, "y1": 0.0, "x2": 2147.4, "y2": 10.0, "gap": 1.0}),
            "x1",
        ),
        (
            "route_differential_pair",
            json!({"board": b, "net_pos": "P", "net_neg": "N",
                   "x1": 0.0, "y1": 0.0, "x2": 10.0, "y2": 0.0, "gap": -1.0}),
            "gap",
        ),
        (
            "route_differential_pair",
            json!({"board": b, "net_pos": "P", "net_neg": "N",
                   "x1": 0.0, "y1": 0.0, "x2": 10.0, "y2": 0.0, "width": 1e-7}),
            "width",
        ),
    ];
    for (name, args, field) in cases {
        assert_refused_unchanged(&p, &p.board, name, args, field).await;
    }
}

#[tokio::test]
async fn board_coordinates_at_the_kicad_limit_are_accepted() {
    let p = project().await;
    let b = p.board.to_str().unwrap();
    let (is_error, body) = call(
        &p.handler,
        "add_board_text",
        json!({"board": b, "text": "EDGE", "x": -2147.483647, "y": 2147.483647}),
    )
    .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["x"], -2147.483647);
}

#[tokio::test]
async fn rotate_component_reports_the_rotation_kicad_stores() {
    let p = project().await;
    let (is_error, body) = call(
        &p.handler,
        "rotate_component",
        json!({"board": p.board, "reference": "R1", "rotation": 725.0}),
    )
    .await;
    assert!(!is_error, "{body}");
    assert_eq!(body["rotation"], 5.0, "{body}");
    let saved = std::fs::read_to_string(&p.board).unwrap();
    assert!(saved.contains("(at 100 50 5)"), "{saved}");
}

#[tokio::test]
async fn create_netclass_refuses_non_physical_dimensions() {
    let p = project().await;
    let b = p.board.to_str().unwrap();
    for (args, field) in [
        (json!({"clearance": -1.0}), "clearance"),
        (json!({"trace_width": 0.0}), "trace_width"),
        (json!({"via_drill": 0.0}), "via_drill"),
        (json!({"via_diameter": -0.8}), "via_diameter"),
        (json!({"diff_pair_width": 0.0}), "diff_pair_width"),
        (json!({"trace_width": "wide"}), "trace_width"),
        (json!({"trace_width": 1e-7}), "trace_width"),
        (json!({"clearance": 2147.483648}), "clearance"),
    ] {
        let mut args = args;
        args["board"] = json!(b);
        args["name"] = json!("Bad");
        assert_refused_unchanged(&p, &p.pro, "create_netclass", args, field).await;
    }
}

#[tokio::test]
async fn create_netclass_accepts_zero_clearance() {
    let p = project().await;
    let (is_error, body) = call(
        &p.handler,
        "create_netclass",
        json!({"board": p.board, "name": "Tight", "clearance": 0.0, "trace_width": 0.1}),
    )
    .await;
    assert!(!is_error, "{body}");
    let pro: Value = serde_json::from_str(&std::fs::read_to_string(&p.pro).unwrap()).unwrap();
    let class = pro["net_settings"]["classes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Tight")
        .cloned()
        .unwrap();
    assert_eq!(class["clearance"], 0.0);
    assert_eq!(class["track_width"], 0.1);
}

#[tokio::test]
async fn add_board_text_refuses_empty_text_and_non_positive_size() {
    let p = project().await;
    let b = p.board.to_str().unwrap();
    for (args, field) in [
        (json!({"text": "", "x": 1.0, "y": 1.0}), "text"),
        (json!({"text": "   ", "x": 1.0, "y": 1.0}), "text"),
        (
            json!({"text": "T", "x": 1.0, "y": 1.0, "size": "big"}),
            "size",
        ),
        (
            json!({"text": "T", "x": 1.0, "y": 1.0, "size": -1.0}),
            "size",
        ),
        (
            json!({"text": "T", "x": 1.0, "y": 1.0, "size": 0.0}),
            "size",
        ),
        (
            json!({"text": "T", "x": 1.0, "y": 1.0, "size": 1e-7}),
            "size",
        ),
    ] {
        let mut args = args;
        args["board"] = json!(b);
        assert_refused_unchanged(&p, &p.board, "add_board_text", args, field).await;
    }
}

fn gr_text_block<'a>(saved: &'a str, text: &str) -> &'a str {
    let start = saved
        .find(&format!("(gr_text \"{text}\""))
        .unwrap_or_else(|| panic!("no gr_text {text}: {saved}"));
    let end = saved[start..].find("(uuid").unwrap() + start;
    &saved[start..end]
}

#[tokio::test]
async fn add_board_text_mirrors_back_layer_text_only() {
    let p = project().await;
    let b = p.board.to_str().unwrap();
    for (text, layer) in [("BOTTOM", "B.SilkS"), ("BCU", "B.Cu"), ("TOP", "F.SilkS")] {
        let (is_error, body) = call(
            &p.handler,
            "add_board_text",
            json!({"board": b, "text": text, "x": 5.0, "y": 5.0, "layer": layer}),
        )
        .await;
        assert!(!is_error, "{body}");
    }
    let saved = std::fs::read_to_string(&p.board).unwrap();
    assert!(gr_text_block(&saved, "BOTTOM").contains("(justify mirror)"));
    assert!(gr_text_block(&saved, "BCU").contains("(justify mirror)"));
    assert!(!gr_text_block(&saved, "TOP").contains("mirror"));
    konnect_sexp::parse_sexp(&saved).unwrap();
}
