//! Optional KiCad 10 native Specctra export through Konnect's legacy ActionPlugin.
//!
//! The bridge is deliberately capability-scoped. It may export the active
//! board into its own private temporary directory; it cannot execute arbitrary
//! Python or write a caller-selected path. KiCad 11 and installations without
//! the legacy plugin continue through Konnect's Rust exporter.

use anyhow::{bail, Context, Result};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PROTOCOL_VERSION: u32 = 1;
const MAX_DSN_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct NativeExport {
    pub dsn: String,
    pub plugin_pid: u32,
    pub protocol_version: u32,
}

#[derive(Debug, Default)]
pub(crate) struct NativeExportAttempt {
    pub export: Option<NativeExport>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    protocol_version: u32,
    pid: u32,
    address: String,
    token: String,
    started_at_unix: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusResponse {
    success: bool,
    protocol_version: u32,
    pid: u32,
    native_specctra_export: bool,
}

#[derive(Debug, Serialize)]
struct ExportRequest<'a> {
    expected_board: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportResponse {
    success: bool,
    protocol_version: u32,
    pid: u32,
    board_path: String,
    dsn_path: String,
    dsn_bytes: u64,
}

pub(crate) async fn try_export(board: &Path) -> NativeExportAttempt {
    let mut attempt = NativeExportAttempt::default();
    let Some(root) = registration_root() else {
        attempt
            .diagnostics
            .push("native bridge registration directory is unavailable".to_string());
        return attempt;
    };
    let entries = match registrations(&root) {
        Ok(entries) => entries,
        Err(error) => {
            attempt
                .diagnostics
                .push(format!("native bridge discovery failed: {error:#}"));
            return attempt;
        }
    };
    if entries.is_empty() {
        attempt
            .diagnostics
            .push("no enabled KiCad 10 native bridge registration was found".to_string());
        return attempt;
    }
    let board = match board.canonicalize() {
        Ok(board) => board,
        Err(error) => {
            attempt
                .diagnostics
                .push(format!("resolve requested board failed: {error}"));
            return attempt;
        }
    };
    for registration_path in entries {
        match try_registration(&root, &registration_path, &board).await {
            Ok(export) => {
                attempt.export = Some(export);
                return attempt;
            }
            Err(error) => attempt.diagnostics.push(format!(
                "{}: {error:#}",
                registration_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("native bridge registration")
            )),
        }
    }
    attempt
}

fn registration_root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KONNECT_BRIDGE_DIR") {
        return Some(PathBuf::from(path));
    }
    dirs::data_local_dir().map(|path| path.join("konnect").join("native-bridge"))
}

fn registrations(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = std::fs::read_dir(root)
        .with_context(|| format!("read native bridge directory {}", root.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("bridge-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries)
}

async fn try_registration(
    root: &Path,
    registration_path: &Path,
    board: &Path,
) -> Result<NativeExport> {
    let registration_source = std::fs::read_to_string(registration_path)
        .with_context(|| format!("read {}", registration_path.display()))?;
    let registration: Registration =
        serde_json::from_str(&registration_source).context("parse bridge registration")?;
    validate_registration(&registration)?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(35))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build native bridge HTTP client")?;
    let status: StatusResponse = client
        .get(format!("{}/v1/status", registration.address))
        .bearer_auth(&registration.token)
        .send()
        .await
        .context("query native bridge status")?
        .error_for_status()
        .context("native bridge status refused")?
        .json()
        .await
        .context("parse native bridge status")?;
    if !status.success
        || !status.native_specctra_export
        || status.protocol_version != PROTOCOL_VERSION
        || status.pid != registration.pid
    {
        bail!("native bridge status does not match its registration");
    }

    let board_text = board
        .to_str()
        .with_context(|| format!("board path is not UTF-8: {}", board.display()))?;
    let response: ExportResponse = client
        .post(format!("{}/v1/export-specctra-dsn", registration.address))
        .bearer_auth(&registration.token)
        .json(&ExportRequest {
            expected_board: board_text,
        })
        .send()
        .await
        .context("request native Specctra export")?
        .error_for_status()
        .context("native Specctra export refused")?
        .json()
        .await
        .context("parse native Specctra export response")?;
    validate_response(&registration, &response, board)?;

    let dsn_path = PathBuf::from(&response.dsn_path)
        .canonicalize()
        .with_context(|| format!("resolve native DSN {}", response.dsn_path))?;
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("resolve bridge root {}", root.display()))?;
    if !dsn_path.starts_with(&canonical_root) {
        bail!("native bridge returned a DSN outside its private directory");
    }
    let metadata = std::fs::metadata(&dsn_path)
        .with_context(|| format!("inspect native DSN {}", dsn_path.display()))?;
    if metadata.len() == 0 || metadata.len() > MAX_DSN_BYTES || metadata.len() != response.dsn_bytes
    {
        bail!("native bridge DSN size does not match its response");
    }
    let dsn = std::fs::read_to_string(&dsn_path)
        .with_context(|| format!("read native DSN {}", dsn_path.display()));
    let cleanup = std::fs::remove_file(&dsn_path)
        .with_context(|| format!("remove consumed native DSN {}", dsn_path.display()));
    let dsn = dsn?;
    cleanup?;
    if !dsn.trim_start().starts_with("(pcb ") {
        bail!("KiCad native export is not a Specctra PCB");
    }
    Ok(NativeExport {
        dsn,
        plugin_pid: response.pid,
        protocol_version: response.protocol_version,
    })
}

fn validate_registration(registration: &Registration) -> Result<()> {
    if registration.protocol_version != PROTOCOL_VERSION
        || registration.pid == 0
        || registration.token.len() < 32
        || !registration.started_at_unix.is_finite()
        || registration.started_at_unix <= 0.0
    {
        bail!("native bridge registration is invalid or incompatible");
    }
    let url = Url::parse(&registration.address).context("parse native bridge address")?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("native bridge address is not a plain IPv4 loopback endpoint");
    }
    Ok(())
}

fn validate_response(
    registration: &Registration,
    response: &ExportResponse,
    board: &Path,
) -> Result<()> {
    if !response.success
        || response.protocol_version != registration.protocol_version
        || response.pid != registration.pid
        || !Path::new(&response.dsn_path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("dsn"))
    {
        bail!("native bridge export response is invalid");
    }
    let active_board = Path::new(&response.board_path)
        .canonicalize()
        .with_context(|| format!("resolve active bridge board {}", response.board_path))?;
    let requested_board = board
        .canonicalize()
        .with_context(|| format!("resolve requested bridge board {}", board.display()))?;
    if active_board != requested_board {
        bail!(
            "native bridge board '{}' does not match requested board '{}'",
            active_board.display(),
            requested_board.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn registration_requires_exact_loopback_shape() {
        let valid = Registration {
            protocol_version: 1,
            pid: 42,
            address: "http://127.0.0.1:32123".to_string(),
            token: "x".repeat(32),
            started_at_unix: 1.0,
        };
        validate_registration(&valid).unwrap();
        for address in [
            "https://127.0.0.1:32123",
            "http://localhost:32123",
            "http://127.0.0.1:32123/path",
            "http://example.com:32123",
        ] {
            let invalid = Registration {
                address: address.to_string(),
                ..Registration {
                    protocol_version: valid.protocol_version,
                    pid: valid.pid,
                    address: valid.address.clone(),
                    token: valid.token.clone(),
                    started_at_unix: valid.started_at_unix,
                }
            };
            assert!(validate_registration(&invalid).is_err(), "{address}");
        }
    }

    #[test]
    fn discovery_is_empty_for_a_missing_directory() {
        let temp = tempfile::tempdir().unwrap();
        assert!(registrations(&temp.path().join("missing"))
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn authenticated_registration_returns_and_consumes_private_dsn() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("native-bridge");
        let session = root.join("session-42-test");
        std::fs::create_dir_all(&session).unwrap();
        let board = temp.path().join("board.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb)\n").unwrap();
        let dsn = session.join("native-test.dsn");
        let dsn_source = "(pcb native-test)\n";
        std::fs::write(&dsn, dsn_source).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = "t".repeat(32);
        let board_text = board.canonicalize().unwrap().display().to_string();
        let dsn_text = dsn.canonicalize().unwrap().display().to_string();
        let server = tokio::spawn(async move {
            for response in [
                serde_json::json!({
                    "success": true,
                    "protocol_version": 1,
                    "pid": 42,
                    "native_specctra_export": true
                }),
                serde_json::json!({
                    "success": true,
                    "protocol_version": 1,
                    "pid": 42,
                    "board_path": board_text,
                    "dsn_path": dsn_text,
                    "dsn_bytes": dsn_source.len()
                }),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 16 * 1024];
                let count = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                assert!(request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer tttttttttttttttttttttttttttttttt"));
                let body = response.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let registration_path = root.join("bridge-42.json");
        std::fs::write(
            &registration_path,
            serde_json::json!({
                "protocol_version": 1,
                "pid": 42,
                "address": format!("http://127.0.0.1:{port}"),
                "token": token,
                "started_at_unix": 1.0
            })
            .to_string(),
        )
        .unwrap();

        let export = try_registration(&root, &registration_path, &board)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(export.dsn, dsn_source);
        assert_eq!(export.plugin_pid, 42);
        assert!(
            !dsn.exists(),
            "Rust must consume the private bridge artifact"
        );
    }

    fn coverage_registration() -> Registration {
        Registration {
            protocol_version: 1,
            pid: 42,
            address: "http://127.0.0.1:32123".into(),
            token: "fixture".repeat(8),
            started_at_unix: 1.0,
        }
    }

    #[test]
    fn registration_refuses_incompatible_identity_credentials_and_url_components() {
        for field in ["protocol", "pid", "token", "nonfinite", "timestamp"] {
            let mut registration = coverage_registration();
            match field {
                "protocol" => registration.protocol_version = 2,
                "pid" => registration.pid = 0,
                "token" => registration.token.clear(),
                "nonfinite" => registration.started_at_unix = f64::NAN,
                "timestamp" => registration.started_at_unix = 0.0,
                _ => unreachable!(),
            }
            assert!(
                validate_registration(&registration)
                    .unwrap_err()
                    .to_string()
                    .contains("invalid or incompatible"),
                "{field}"
            );
        }
        for address in [
            "http://127.0.0.1",
            "http://127.0.0.1:32123/?token=other",
            "http://127.0.0.1:32123/#redirect",
            "http://user@127.0.0.1:32123",
            "http://:password@127.0.0.1:32123",
            "not a URL",
        ] {
            let registration = Registration {
                address: address.into(),
                ..coverage_registration()
            };
            assert!(validate_registration(&registration).is_err(), "{address}");
        }
    }

    #[test]
    fn discovery_only_returns_sorted_registration_names() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "other.json",
            "bridge-3.txt",
            "bridge-2.json",
            "bridge-1.json",
        ] {
            std::fs::write(dir.path().join(name), "{}").unwrap();
        }
        assert_eq!(
            registrations(dir.path()).unwrap(),
            vec![
                dir.path().join("bridge-1.json"),
                dir.path().join("bridge-2.json")
            ]
        );
    }

    #[test]
    fn export_response_refuses_wrong_status_identity_extension_and_board() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        let other = dir.path().join("other.kicad_pcb");
        std::fs::write(&board, "board").unwrap();
        std::fs::write(&other, "board").unwrap();
        for field in ["success", "protocol", "pid", "extension", "board"] {
            let mut response = ExportResponse {
                success: true,
                protocol_version: 1,
                pid: 42,
                board_path: board.display().to_string(),
                dsn_path: "export.DSN".into(),
                dsn_bytes: 1,
            };
            match field {
                "success" => response.success = false,
                "protocol" => response.protocol_version = 2,
                "pid" => response.pid = 43,
                "extension" => response.dsn_path = "export.txt".into(),
                "board" => response.board_path = other.display().to_string(),
                _ => unreachable!(),
            }
            let error = validate_response(&coverage_registration(), &response, &board).unwrap_err();
            let expected = if field == "board" {
                "does not match requested board"
            } else {
                "response is invalid"
            };
            assert!(error.to_string().contains(expected), "{field}: {error:#}");
        }
    }

    async fn coverage_bridge_server(
        root: &Path,
        board: &Path,
        responses: Vec<serde_json::Value>,
    ) -> (PathBuf, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let expected_board = board.to_str().unwrap().to_owned();
        let task = tokio::spawn(async move {
            for (index, response) in responses.into_iter().enumerate() {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let (request, request_body) = tokio::time::timeout(Duration::from_secs(3), async {
                    let mut request = Vec::new();
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let mut chunk = [0u8; 1024];
                        let count = stream.read(&mut chunk).await.unwrap();
                        assert!(count > 0, "client closed before sending headers");
                        request.extend_from_slice(&chunk[..count]);
                        assert!(request.len() <= 8192, "request exceeded fixture bound");
                    }
                    let header_end = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .unwrap()
                        + 4;
                    let headers = std::str::from_utf8(&request[..header_end])
                        .unwrap()
                        .to_string();
                    let content_length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
                    let request_length = header_end.checked_add(content_length).unwrap();
                    assert!(
                        request_length <= 8192,
                        "request body exceeded fixture bound"
                    );
                    while request.len() < request_length {
                        let mut chunk = [0u8; 1024];
                        let limit = (request_length - request.len()).min(chunk.len());
                        let count = stream.read(&mut chunk[..limit]).await.unwrap();
                        assert!(count > 0, "client closed before sending the complete body");
                        request.extend_from_slice(&chunk[..count]);
                    }
                    assert_eq!(request.len(), request_length);
                    (headers, request[header_end..].to_vec())
                })
                .await
                .unwrap();
                let method = if index == 0 {
                    "GET /v1/status "
                } else {
                    "POST /v1/export-specctra-dsn "
                };
                assert!(request.starts_with(method), "{request}");
                if index == 0 {
                    assert!(request_body.is_empty());
                } else {
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&request_body).unwrap(),
                        serde_json::json!({"expected_board": expected_board})
                    );
                }
                assert!(request
                    .to_ascii_lowercase()
                    .contains(&format!("authorization: bearer {}", "fixture".repeat(8))));
                let body = response.to_string();
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                tokio::time::timeout(Duration::from_secs(3), stream.write_all(reply.as_bytes()))
                    .await
                    .unwrap()
                    .unwrap();
            }
        });
        let registration = root.join("bridge-42.json");
        std::fs::write(
            &registration,
            serde_json::json!({
                "protocol_version": 1, "pid":42, "address": format!("http://{address}"),
                "token": "fixture".repeat(8), "started_at_unix":1.0,
            })
            .to_string(),
        )
        .unwrap();
        (registration, task)
    }

    #[tokio::test]
    async fn mismatched_live_status_never_requests_an_export() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, "board").unwrap();
        for (key, value) in [
            ("success", json!(false)),
            ("native_specctra_export", json!(false)),
            ("protocol_version", json!(2)),
            ("pid", json!(43)),
        ] {
            let mut status =
                json!({"success":true,"native_specctra_export":true,"protocol_version":1,"pid":42});
            status[key] = value;
            let (registration, server) =
                coverage_bridge_server(dir.path(), &board, vec![status]).await;
            let error = try_registration(dir.path(), &registration, &board)
                .await
                .unwrap_err();
            server.await.unwrap();
            assert!(
                error.to_string().contains("status does not match"),
                "{key}: {error:#}"
            );
        }
    }

    #[tokio::test]
    async fn private_artifact_validation_refuses_foreign_missing_and_inconsistent_output() {
        use serde_json::json;
        for case in [
            "outside",
            "missing",
            "empty",
            "wrong_size",
            "oversized",
            "wrong_root",
            "invalid_utf8",
            "uppercase",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("bridge");
            std::fs::create_dir(&root).unwrap();
            let board = dir.path().join("board.kicad_pcb");
            std::fs::write(&board, "board").unwrap();
            let dsn = if case == "outside" {
                dir.path().join("foreign.dsn")
            } else {
                root.join("private.DSN")
            };
            let bytes: &[u8] = match case {
                "empty" => b"",
                "wrong_root" => b"(session board)",
                "invalid_utf8" => &[0xff],
                _ => b"(pcb board)",
            };
            if case != "missing" {
                std::fs::write(&dsn, bytes).unwrap();
            }
            if case == "oversized" {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&dsn)
                    .unwrap()
                    .set_len(MAX_DSN_BYTES + 1)
                    .unwrap();
            }
            let size = if case == "oversized" {
                MAX_DSN_BYTES + 1
            } else if case == "wrong_size" {
                bytes.len() as u64 + 1
            } else {
                bytes.len() as u64
            };
            let (registration, server) = coverage_bridge_server(&root, &board, vec![
                json!({"success":true,"native_specctra_export":true,"protocol_version":1,"pid":42}),
                json!({"success":true,"protocol_version":1,"pid":42,"board_path":board,"dsn_path":dsn,"dsn_bytes":size}),
            ]).await;
            let result = try_registration(&root, &registration, &board).await;
            server.await.unwrap();
            if case == "uppercase" {
                assert_eq!(result.unwrap().dsn, "(pcb board)");
                assert!(!dsn.exists());
                continue;
            }
            let expected = match case {
                "outside" => "outside its private directory",
                "missing" => "resolve native DSN",
                "wrong_root" => "not a Specctra PCB",
                "invalid_utf8" => "read native DSN",
                _ => "size does not match",
            };
            let error = format!("{:#}", result.unwrap_err());
            assert!(error.contains(expected), "{case}: {error}");
            if matches!(case, "wrong_root" | "invalid_utf8") {
                assert!(
                    !dsn.exists(),
                    "consumed private artifacts are removed even if content is invalid"
                );
            } else if case != "missing" {
                assert!(
                    dsn.exists(),
                    "unaccepted artifact must not be deleted: {case}"
                );
            }
            assert_eq!(std::fs::read_to_string(board).unwrap(), "board");
        }
    }
}
