//! A real stdio request must honor launch readiness deadlines.

#![cfg(unix)]

use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[tokio::test]
async fn slow_ipc_observation_keeps_the_served_readiness_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("api");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let stalled = tokio::spawn(async move {
        let (_connection, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await;
    });
    let config = temp.path().join("konnect.toml");
    std::fs::write(
        &config,
        format!(
            "ipc_address = 'ipc://{}'\nauto_load_toolsets = true\n",
            socket.display()
        ),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_konnect"))
        .args(["--config", config.to_str().unwrap()])
        .env("HOME", temp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();

    for (request, limit) in [
        (
            json!({"id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "launch-deadline", "version": "1"}
            }}),
            Duration::from_secs(5),
        ),
        (
            json!({"id": 2, "method": "tools/call", "params": {
                "name": "launch_kicad_ui", "arguments": {
                    "board": temp.path().join("requested.kicad_pcb"),
                    "timeout_seconds": 1
                }
            }}),
            Duration::from_millis(1800),
        ),
    ] {
        let mut request = request;
        request["jsonrpc"] = json!("2.0");
        input
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
        let response = tokio::time::timeout(limit, async {
            loop {
                let line = output.next_line().await.unwrap().unwrap();
                let response: Value = serde_json::from_str(&line).unwrap();
                if response["id"] == request["id"] {
                    break response;
                }
            }
        })
        .await
        .expect("the served response must honor the deadline");
        if request["id"] == 2 {
            let result = &response["result"];
            assert_eq!(result["isError"], false, "{response}");
            let report: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(report["timed_out"], true, "{report}");
            assert!(report["requested_open"].is_null(), "{report}");
        }
    }
    child.kill().await.unwrap();
    stalled.abort();
}
