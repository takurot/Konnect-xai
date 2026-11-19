//! Served KiCad settings discovery with isolated user directories.

#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

#[test]
fn nightly_cli_selects_global_tables_and_path_variables() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("kicad");
    let stable = base.join("10.0");
    let nightly = base.join("10.99");
    fs::create_dir_all(&stable).unwrap();
    fs::create_dir_all(&nightly).unwrap();
    let stable_table = "(fp_lib_table (version 7))\n";
    fs::write(stable.join("fp-lib-table"), stable_table).unwrap();

    let parts = tmp.path().join("Fixture.pretty");
    fs::create_dir(&parts).unwrap();
    fs::write(parts.join("Nightly.kicad_mod"), "(footprint \"Nightly\")").unwrap();
    fs::write(
        nightly.join("kicad_common.json"),
        json!({"environment": {"vars": {"NIGHTLY_FIXTURE": parts}}}).to_string(),
    )
    .unwrap();
    fs::write(
        nightly.join("nested-table"),
        r#"(fp_lib_table (version 7) (lib (name "Fixture") (type "KiCad") (uri "${NIGHTLY_FIXTURE}") (options "") (descr "")))"#,
    )
    .unwrap();
    fs::write(
        nightly.join("fp-lib-table"),
        format!(
            r#"(fp_lib_table (version 7) (lib (name "Nested") (type "Table") (uri "{}") (options "") (descr "")))"#,
            nightly.join("nested-table").display()
        ),
    )
    .unwrap();
    let cli = tmp.path().join("fixture-kicad-cli");
    fs::write(
        &cli,
        "#!/bin/sh\n[ \"$1\" = --version ] && echo 10.99.0-127-gabcd\n",
    )
    .unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
    let config = tmp.path().join("konnect.toml");
    fs::write(
        &config,
        format!(
            "kicad_cli = '{}'\nauto_load_toolsets = true\n",
            cli.display()
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_konnect"))
        .args(["--config", config.to_str().unwrap()])
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("KICAD_CONFIG_HOME", &base)
        .env_remove("NIGHTLY_FIXTURE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let notify = |stdin: &mut std::process::ChildStdin, request: Value| {
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
    };
    let mut request = |method, id, params: Value| {
        notify(
            &mut stdin,
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        );
        loop {
            let mut line = String::new();
            assert_ne!(output.read_line(&mut line).unwrap(), 0, "{method}");
            let response: Value = serde_json::from_str(&line).unwrap();
            if response["id"] == id {
                break response["result"].clone();
            }
        }
    };
    request(
        "initialize",
        1,
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"discovery-test","version":"1"}}),
    );
    let search = request(
        "tools/call",
        2,
        json!({"name":"search_footprints","arguments":{"query":"Nightly"}}),
    );
    let registration = request(
        "tools/call",
        3,
        json!({"name":"register_footprint_library","arguments":{
            "library_path":parts,"nickname":"NewFixture","scope":"global"
        }}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());

    let body = |result: Value| -> Value {
        assert_ne!(result["isError"], true, "{result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    let search = body(search);
    assert_eq!(search["results"][0]["id"], "Fixture:Nightly", "{search}");
    let registration = body(registration);
    assert_eq!(registration["table"], json!(nightly.join("fp-lib-table")));
    assert!(fs::read_to_string(nightly.join("fp-lib-table"))
        .unwrap()
        .contains("NewFixture"));
    assert_eq!(
        fs::read_to_string(stable.join("fp-lib-table")).unwrap(),
        stable_table
    );
}
