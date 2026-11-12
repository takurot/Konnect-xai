//! Exercise cancellation across the real stdio, tool dispatcher and CLI child.
#![cfg(unix)]

use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::{timeout, Duration};

async fn send(stdin: &mut tokio::process::ChildStdin, message: Value) {
    stdin
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
}

async fn response(lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) -> Value {
    loop {
        let line = timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let message: Value = serde_json::from_str(&line).unwrap();
        if message.get("id").is_some() {
            return message;
        }
    }
}

fn process_exists(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn cancellation_kills_cli_keeps_ping_responsive_and_never_sends_cancelled_result() {
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("fake-cli");
    std::fs::write(
        &cli,
        "#!/bin/sh\ncase \"$1\" in --version|-v|version) printf '10.0.8\\n'; exit 0;; esac\n[ \"$1 $2 $3\" = \"sch export pdf\" ] || exit 2\nprintf '%s' \"$$\" > \"$KONNECT_TEST_PID\"\nexec sleep 60\n",
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.path().join("konnect.toml"),
        format!(
            "kicad_cli = {:?}\nauto_load_toolsets = true\n",
            cli.to_str().unwrap()
        ),
    )
    .unwrap();
    let schematic = dir.path().join("demo.kicad_sch");
    std::fs::write(
        &schematic,
        "(kicad_sch (version 20250114) (generator eeschema))",
    )
    .unwrap();
    let pid_file = dir.path().join("cli.pid");
    let probe = std::process::Command::new(&cli)
        .arg("--version")
        .env("KONNECT_TEST_PID", &pid_file)
        .output()
        .unwrap();
    assert!(probe.status.success());
    assert!(
        !pid_file.exists(),
        "discovery probes must not impersonate the export child"
    );
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_konnect"))
        .current_dir(dir.path())
        .env("KONNECT_TEST_PID", &pid_file)
        .env("KONNECT_STATE_DIR", dir.path().join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    send(&mut stdin, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"cancellation-test","version":"0"}}})).await;
    assert_eq!(response(&mut lines).await["id"], 1);
    send(&mut stdin, json!({"jsonrpc":"2.0","id":"export","method":"tools/call","params":{"name":"export_schematic_pdf","arguments":{"schematic":schematic,"output":dir.path().join("demo.pdf")}}})).await;
    let pid = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file) {
                if !pid.is_empty() {
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("export must start fake CLI");
    send(&mut stdin, json!({"jsonrpc":"2.0","id":3,"method":"ping"})).await;
    assert_eq!(response(&mut lines).await["id"], 3);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"export"}}),
    )
    .await;
    send(&mut stdin, json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_toolboxes","arguments":{}}})).await;
    assert_eq!(
        response(&mut lines).await["id"],
        4,
        "cancelled export must not respond and queued tool must resume"
    );
    let stopped = timeout(Duration::from_secs(5), async {
        while process_exists(&pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok();
    let process_state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .unwrap_or_else(|error| format!("process state unavailable: {error}"));
    if !stopped {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid])
            .status();
    }
    assert!(
        stopped,
        "cancelled CLI {pid} must terminate and be reaped: {process_state}"
    );
    stdin.shutdown().await.unwrap();
    // ChildStdin owns a pipe; close its descriptor to deliver EOF.
    drop(stdin);
    assert!(timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    while let Some(line) = lines.next_line().await.unwrap() {
        let message: Value = serde_json::from_str(&line).unwrap();
        assert_ne!(message["id"], "export", "cancelled response arrived late");
    }
    assert!(!dir.path().join("demo.pdf").exists());
}
