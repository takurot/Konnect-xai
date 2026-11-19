//! Served `.kicad_pro` writer contracts, including the KiCad ownership guard (#791).
//!
//! Saving the board in KiCad rewrites the project file from KiCad's own copy.
//! So `set_design_rules`, `create_netclass` and `assign_net_to_class` must not
//! write while KiCad holds the board: the write would be reported and then
//! reverted. Measured on KiCad 10.0.5 before the fix: all three reported
//! success, and `run_drc` with `sync_live_board` then put every value back.
//!
//! Driven through served `tools/call`, so dispatch, schema and handler are all
//! under test. The project is the KiCad-written placement fixture
//! (`konnect-sexp/tests/fixtures/placement/`).

use crate::mcp::handler::McpHandler;
use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::tools::pcb_board::board_mock::spawn_kicad_holding_boards;
use crate::tools::ServerConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../konnect-sexp/tests/fixtures/placement"
);

/// A copy of the fixture project, kept alive for the test's duration.
struct Project {
    _dir: tempfile::TempDir,
    board: PathBuf,
    project_file: PathBuf,
}

fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    for extension in ["kicad_pcb", "kicad_pro"] {
        std::fs::copy(
            Path::new(FIXTURE_DIR).join(format!("placement_fixture.{extension}")),
            dir.path().join(format!("placement_fixture.{extension}")),
        )
        .unwrap();
    }
    Project {
        board: dir.path().join("placement_fixture.kicad_pcb"),
        project_file: dir.path().join("placement_fixture.kicad_pro"),
        _dir: dir,
    }
}

async fn handler_talking_to(address: &str) -> McpHandler {
    McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: address.to_string(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: true,
    })
    .await
    .expect("handler builds")
}

async fn call(handler: &McpHandler, tool: &str, board: &Path, arguments: &Value) -> CallToolResult {
    let mut arguments = arguments.clone();
    arguments["board"] = json!(board.to_string_lossy());
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }))
        .await
        .expect("tools/call receives a response");
    assert!(response.error.is_none(), "tool errors are MCP results");
    serde_json::from_value(response.result.expect("a result"))
        .expect("result uses the advertised MCP shape")
}

fn text_of(result: &CallToolResult) -> &str {
    match result.content.first() {
        Some(ToolContent::Text { text }) => text,
        other => panic!("expected text content, got {other:?}"),
    }
}

/// Arguments that change something the fixture's project file does not
/// already hold (its rules say 0.2, it has only `Default`, and no net is
/// assigned), so a write that happened would show in the file.
fn arguments_for(tool: &str) -> Value {
    match tool {
        "set_design_rules" => json!({ "min_trace_width": 0.35 }),
        "create_netclass" => json!({ "name": "Power", "trace_width": 0.5 }),
        "assign_net_to_class" => json!({ "net_name": "VCC", "netclass": "Default" }),
        other => panic!("no arguments for {other}"),
    }
}

/// KiCad holds this very board: the tool refuses, and neither the project
/// file nor the board changes by a byte.
async fn assert_refused_while_kicad_holds_the_board(tool: &str) {
    let project = project();
    let project_before = std::fs::read(&project.project_file).unwrap();
    let board_before = std::fs::read(&project.board).unwrap();
    let kicad = spawn_kicad_holding_boards(&[&project.board], |_| None);
    let handler = handler_talking_to(kicad.address()).await;

    let result = call(&handler, tool, &project.board, &arguments_for(tool)).await;

    assert!(
        result.is_error,
        "{tool} must refuse while KiCad holds the board: {}",
        text_of(&result)
    );
    assert!(
        text_of(&result).contains("currently holds this board open"),
        "{tool} must say why it refused: {}",
        text_of(&result)
    );
    assert_eq!(
        std::fs::read(&project.project_file).unwrap(),
        project_before,
        "{tool} wrote the project file while KiCad held the board"
    );
    assert_eq!(std::fs::read(&project.board).unwrap(), board_before);
}

#[tokio::test]
async fn set_design_rules_refuses_while_kicad_holds_the_board() {
    assert_refused_while_kicad_holds_the_board("set_design_rules").await;
}

#[tokio::test]
async fn create_netclass_refuses_while_kicad_holds_the_board() {
    assert_refused_while_kicad_holds_the_board("create_netclass").await;
}

#[tokio::test]
async fn assign_net_to_class_refuses_while_kicad_holds_the_board() {
    assert_refused_while_kicad_holds_the_board("assign_net_to_class").await;
}

/// The control: a KiCad holding some other board does not own this project
/// file, so each writer still writes, and the file says so.
#[tokio::test]
async fn each_writer_still_writes_while_kicad_holds_a_different_board() {
    let elsewhere = tempfile::tempdir().unwrap();
    let other_board = elsewhere.path().join("other.kicad_pcb");
    std::fs::copy(
        Path::new(FIXTURE_DIR).join("placement_fixture.kicad_pcb"),
        &other_board,
    )
    .unwrap();

    for tool in ["set_design_rules", "create_netclass", "assign_net_to_class"] {
        let project = project();
        let kicad = spawn_kicad_holding_boards(&[&other_board], |_| None);
        let handler = handler_talking_to(kicad.address()).await;

        let result = call(&handler, tool, &project.board, &arguments_for(tool)).await;
        assert!(!result.is_error, "{tool}: {}", text_of(&result));

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&project.project_file).unwrap()).unwrap();
        let net_settings = &written["net_settings"];
        match tool {
            "set_design_rules" => assert_eq!(
                written["board"]["design_settings"]["rules"]["min_track_width"],
                json!(0.35)
            ),
            "create_netclass" => assert!(net_settings["classes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|class| class["name"] == "Power" && class["track_width"] == json!(0.5))),
            "assign_net_to_class" => assert_eq!(
                net_settings["netclass_patterns"],
                json!([{ "netclass": "Default", "pattern": "VCC" }])
            ),
            _ => unreachable!(),
        }
    }
}

/// A project lock exists when Eeschema is open even without a board lock.
/// Locks have no trustworthy PID; presumed-stale locks are also refused.
#[tokio::test]
async fn all_project_writers_refuse_an_eeschema_project_lock() {
    let project = project();
    let before = std::fs::read(&project.project_file).unwrap();
    let lock = project
        .project_file
        .with_file_name("~placement_fixture.kicad_pro.lck");
    std::fs::write(&lock, r#"{"username":"test","hostname":"test-host"}"#).unwrap();
    let handler = handler_talking_to("").await;
    for (tool, arguments) in [
        ("set_design_rules", json!({"min_clearance":0.25})),
        ("set_predefined_sizes", json!({"track_widths":[0.25]})),
        ("create_netclass", json!({"name":"Signal","clearance":0.25})),
        (
            "assign_net_to_class",
            json!({"net_name":"GND","netclass":"Default"}),
        ),
    ] {
        let result = call(&handler, tool, &project.board, &arguments).await;
        assert!(result.is_error, "{tool}: {}", text_of(&result));
        assert!(
            text_of(&result).contains(".kicad_pro.lck"),
            "{tool}: {}",
            text_of(&result)
        );
        assert_eq!(
            std::fs::read(&project.project_file).unwrap(),
            before,
            "{tool}"
        );
        assert!(lock.exists());
    }
}

#[tokio::test]
async fn design_rules_accept_fab_minima_and_refuse_out_of_range_batches() {
    let fixture = project();
    let handler = handler_talking_to("").await;
    let before = std::fs::read(&fixture.project_file).unwrap();
    for (field, value, upper_bound) in [
        ("min_clearance", 30.0, 25),
        ("min_trace_width", -0.1, 25),
        ("min_via_drill", 25.1, 25),
        ("min_via_size", 25.1, 25),
        ("min_hole_to_hole", 10.1, 10),
        ("min_via_annular_width", -0.1, 25),
        ("min_hole_clearance", 100.1, 100),
        ("min_text_height", 100.1, 100),
        ("min_text_thickness", -0.1, 25),
        ("min_silk_clearance", -10.1, 100),
        ("min_copper_edge_clearance", -0.02, 25),
    ] {
        let result = call(
            &handler,
            "set_design_rules",
            &fixture.board,
            &json!({"min_hole_to_hole": 0.25, (field): value}),
        )
        .await;
        assert!(result.is_error, "{field}: {}", text_of(&result));
        let error: Value = serde_json::from_str(text_of(&result)).unwrap();
        assert_eq!(error["error"]["kind"], "invalid_argument");
        assert!(text_of(&result).contains(field), "{}", text_of(&result));
        assert!(text_of(&result).contains(&upper_bound.to_string()));
        assert_eq!(std::fs::read(&fixture.project_file).unwrap(), before);
    }

    let arguments = json!({
        "min_clearance": 0.1,
        "min_via_annular_width": 0.13,
        "min_hole_clearance": 0.2,
        "min_text_height": 0.8,
        "min_text_thickness": 0.15,
        "min_silk_clearance": 0.12,
        "min_copper_edge_clearance": 0.3,
    });
    let result = call(&handler, "set_design_rules", &fixture.board, &arguments).await;
    assert!(!result.is_error, "{}", text_of(&result));
    let set_body: Value = serde_json::from_str(text_of(&result)).unwrap();
    let persisted = std::fs::read(&fixture.project_file).unwrap();
    let repeated = call(&handler, "set_design_rules", &fixture.board, &arguments).await;
    assert!(!repeated.is_error, "{}", text_of(&repeated));
    let repeated_body: Value = serde_json::from_str(text_of(&repeated)).unwrap();
    assert_eq!(repeated_body["changed"], json!([]));
    assert_eq!(std::fs::read(&fixture.project_file).unwrap(), persisted);
    let result = call(&handler, "get_design_rules", &fixture.board, &json!({})).await;
    assert!(!result.is_error, "{}", text_of(&result));
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert_eq!(set_body["rules"], body["rules"]);
    for (field, value) in arguments.as_object().unwrap() {
        assert_eq!(&body["rules"][field], value, "{field}");
    }
}

#[tokio::test]
async fn differential_netclass_fields_roundtrip_and_inherit() {
    let fixture = project();
    let handler = handler_talking_to("").await;
    let result = call(
        &handler,
        "create_netclass",
        &fixture.board,
        &json!({"name":"Default","diff_pair_via_gap":0.65}),
    )
    .await;
    assert!(!result.is_error, "{}", text_of(&result));
    let result = call(
        &handler,
        "create_netclass",
        &fixture.board,
        &json!({"name":"Pair","diff_pair_width":0.18,"diff_pair_gap":0.14}),
    )
    .await;
    assert!(!result.is_error, "{}", text_of(&result));
    let result = call(
        &handler,
        "get_netclasses",
        &fixture.board,
        &json!({"board_source":"saved"}),
    )
    .await;
    assert!(!result.is_error, "{}", text_of(&result));
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    let pair = body["netclasses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|class| class["name"] == "Pair")
        .unwrap();
    assert_eq!(pair["diff_pair_width"], 0.18);
    assert_eq!(pair["diff_pair_gap"], 0.14);
    assert_eq!(pair["diff_pair_via_gap"], 0.65);
    assert_eq!(pair["inherits"], json!(["diff_pair_via_gap"]));
    let written: Value =
        serde_json::from_slice(&std::fs::read(&fixture.project_file).unwrap()).unwrap();
    let stored_pair = written["net_settings"]["classes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|class| class["name"] == "Pair")
        .unwrap();
    assert!(stored_pair.get("diff_pair_via_gap").is_none());
}

#[tokio::test]
async fn delete_netclass_preserves_surviving_matches_and_default_fallback() {
    let fixture = project();
    let mut saved: Value =
        serde_json::from_slice(&std::fs::read(&fixture.project_file).unwrap()).unwrap();
    saved["net_settings"] = json!({
        "meta":{"version":5},
        "classes": [
            {"name":"Default","clearance":0.41},
            {"name":"Other","priority":4,"clearance":0.23},
            {"name":"Remove","priority":1,"clearance":0.91}
        ],
        "netclass_patterns":[
            {"netclass":"Other","pattern":"S*"},
            {"netclass":"Remove","pattern":"SIG"},
            {"netclass":"Other","pattern":"VCC"}
        ],
        "netclass_assignments":{"SIG":["Remove","Other"],"ONLY":["Remove"],"OLD":"Remove"}
    });
    std::fs::write(&fixture.project_file, serde_json::to_vec(&saved).unwrap()).unwrap();
    let handler = handler_talking_to("").await;
    let board_before = std::fs::read(&fixture.board).unwrap();
    let before = std::fs::read(&fixture.project_file).unwrap();
    let result = call(
        &handler,
        "delete_netclass",
        &fixture.board,
        &json!({"name":"Default"}),
    )
    .await;
    assert!(result.is_error, "{}", text_of(&result));
    assert_eq!(std::fs::read(&fixture.project_file).unwrap(), before);

    let locked = spawn_kicad_holding_boards(&[&fixture.board], |_| None);
    let live_handler = handler_talking_to(locked.address()).await;
    let args = json!({"name":"Remove"});
    let result = call(&live_handler, "delete_netclass", &fixture.board, &args).await;
    assert!(result.is_error, "{}", text_of(&result));
    assert!(text_of(&result).contains("currently holds this board open"));
    assert_eq!(std::fs::read(&fixture.project_file).unwrap(), before);
    drop(locked);

    let lock = fixture
        .project_file
        .with_file_name("~placement_fixture.kicad_pro.lck");
    std::fs::write(&lock, r#"{"username":"test","hostname":"test-host"}"#).unwrap();
    let result = call(&handler, "delete_netclass", &fixture.board, &args).await;
    assert!(result.is_error, "{}", text_of(&result));
    assert!(text_of(&result).contains(".kicad_pro.lck"));
    assert_eq!(std::fs::read(&fixture.project_file).unwrap(), before);
    std::fs::remove_file(lock).unwrap();

    let result = call(&handler, "delete_netclass", &fixture.board, &args).await;
    assert!(!result.is_error, "{}", text_of(&result));
    let written: Value =
        serde_json::from_slice(&std::fs::read(&fixture.project_file).unwrap()).unwrap();
    let net_settings = &written["net_settings"];
    assert_eq!(net_settings["classes"].as_array().unwrap().len(), 2);
    assert_eq!(net_settings["classes"][0]["name"], "Default");
    assert_eq!(net_settings["classes"][0]["clearance"], 0.41);
    assert_eq!(net_settings["classes"][0]["wire_width"], 6);
    assert_eq!(
        net_settings["classes"][1],
        saved["net_settings"]["classes"][1]
    );
    assert_eq!(
        net_settings["netclass_patterns"],
        json!([{"netclass":"Other","pattern":"S*"},{"netclass":"Other","pattern":"VCC"}])
    );
    assert_eq!(
        net_settings["netclass_assignments"],
        json!({"SIG":["Other"]})
    );
    assert_eq!(std::fs::read(&fixture.board).unwrap(), board_before);
}
