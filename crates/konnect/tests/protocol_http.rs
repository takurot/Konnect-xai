//! Streamable HTTP transport tests (MCP spec 2025-06-18).
//!
//! Spawns the real binary with `transport = "http"` and asserts the seven
//! behaviors verified by hand at release time, plus the tools/list_changed
//! notification arriving on the SSE stream after load_toolset.
//!
//! Plain std TcpStream HTTP/1.1 — no client dependency needed for requests
//! this simple.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct HttpServer {
    child: Child,
    addr: String,
    _config: tempfile::NamedTempFile,
}

/// Spawn the binary against a temp JSON config, with `KONNECT_HTTP_TOKEN`
/// set to `env_token` or removed.
fn spawn_konnect(
    config_json: &str,
    env_token: Option<&str>,
    stderr: Stdio,
) -> (Child, tempfile::NamedTempFile) {
    let mut config = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
    config.write_all(config_json.as_bytes()).unwrap();
    config.flush().unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_konnect"));
    command
        .arg("--config")
        .arg(config.path())
        .stdin(Stdio::piped()) // piped stdin so it doesn't detect a TTY
        .stdout(Stdio::null())
        .stderr(stderr);
    match env_token {
        Some(token) => command.env("KONNECT_HTTP_TOKEN", token),
        None => command.env_remove("KONNECT_HTTP_TOKEN"),
    };
    let child = command.spawn().expect("failed to spawn konnect binary");
    (child, config)
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

impl HttpServer {
    fn spawn() -> Self {
        Self::spawn_bound("127.0.0.1", "", None)
    }

    /// `extra_config` is spliced into the JSON config object after the
    /// transport and address, e.g. `,"http_token":"t"`.
    fn spawn_bound(bind_ip: &str, extra_config: &str, env_token: Option<&str>) -> Self {
        // The probed port can be taken between the probe and the child's own
        // bind (TOCTOU) — same race that made the konnect-ipc mocks flaky on
        // CI. The child needs a real TCP port (separate process, no inproc
        // option), so handle the race by retrying on a fresh port instead.
        let mut last_err = String::new();
        for _ in 0..5 {
            match Self::try_spawn(bind_ip, extra_config, env_token) {
                Ok(server) => return server,
                Err(e) => last_err = e,
            }
        }
        panic!("server never became ready: {last_err}");
    }

    fn try_spawn(
        bind_ip: &str,
        extra_config: &str,
        env_token: Option<&str>,
    ) -> Result<Self, String> {
        let port = free_port();
        let config_json =
            format!(r#"{{"transport":"http","http_address":"{bind_ip}:{port}"{extra_config}}}"#);
        let (child, config) = spawn_konnect(&config_json, env_token, Stdio::null());
        let addr = format!("127.0.0.1:{port}");

        let mut server = HttpServer {
            child,
            addr,
            _config: config,
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok((status, _, body)) = server.raw_request("GET", "/health", &[], None) {
                if status == 200 && body == "ok" {
                    return Ok(server);
                }
            }
            // Child already gone: its bind lost the port race (or it crashed).
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(format!(
                    "server exited ({status}) before becoming ready on {}",
                    server.addr
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("server did not become ready on {}", server.addr))
    }

    /// Minimal HTTP/1.1 request. Returns (status, headers, body).
    fn raw_request(
        &self,
        method: &str,
        path: &str,
        extra_headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> std::io::Result<(u16, String, String)> {
        let mut stream = TcpStream::connect(&self.addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let body = body.unwrap_or("");
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if !extra_headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("host"))
        {
            req.push_str(&format!("Host: {}\r\n", self.addr));
        }
        if body.starts_with('{') {
            req.push_str("Content-Type: application/json\r\n");
        }
        for (k, v) in extra_headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(body);
        stream.write_all(req.as_bytes())?;

        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status: u16 = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // Bodies may be chunked; strip chunk framing crudely if present.
        let body = if head.to_lowercase().contains("transfer-encoding: chunked") {
            body.lines()
                .filter(|l| !l.chars().all(|c| c.is_ascii_hexdigit()) || l.contains('{'))
                .collect::<Vec<_>>()
                .join("")
        } else {
            body.to_string()
        };
        Ok((status, head.to_string(), body))
    }

    fn post_mcp(&self, msg: &Value) -> (u16, String, Value) {
        let (status, headers, body) = self
            .raw_request(
                "POST",
                "/mcp",
                &[("Accept", "application/json, text/event-stream")],
                Some(&msg.to_string()),
            )
            .unwrap();
        let parsed = serde_json::from_str(&body).unwrap_or(Value::Null);
        (status, headers, parsed)
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialize_msg() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "http-test", "version": "0"}}
    })
}

#[test]
fn streamable_http_conformance() {
    let server = HttpServer::spawn();

    // 1. POST a request → 200 with application/json body.
    let (status, headers, body) = server.post_mcp(&initialize_msg());
    assert_eq!(status, 200);
    assert!(headers.to_lowercase().contains("application/json"));
    assert_eq!(body["result"]["serverInfo"]["name"], "konnect");

    // 2. POST a notification → 202 Accepted, no body.
    let (status, _, _) = server
        .raw_request(
            "POST",
            "/mcp",
            &[("Accept", "application/json, text/event-stream")],
            Some(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
        )
        .unwrap();
    assert_eq!(status, 202);

    // 3. DELETE /mcp → 405 (stateless server, no session termination).
    let (status, _, _) = server.raw_request("DELETE", "/mcp", &[], None).unwrap();
    assert_eq!(status, 405);

    // 4. Non-localhost Origin → 403 (DNS-rebinding protection is MUST).
    let (status, _, _) = server
        .raw_request(
            "POST",
            "/mcp",
            &[("Origin", "https://evil.example.com")],
            Some(r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#),
        )
        .unwrap();
    assert_eq!(status, 403);

    // 5. Localhost Origin → allowed.
    let (status, _, _) = server
        .raw_request(
            "POST",
            "/mcp",
            &[("Origin", "http://localhost:3000")],
            Some(r#"{"jsonrpc":"2.0","id":10,"method":"ping"}"#),
        )
        .unwrap();
    assert_eq!(status, 200);
}

#[test]
fn sse_stream_delivers_tools_list_changed() {
    let server = HttpServer::spawn();
    let _ = server.post_mcp(&initialize_msg());

    // 6. GET /mcp with SSE accept → text/event-stream. Keep the socket open
    //    and read events from it while a second connection mutates state.
    let mut sse = TcpStream::connect(&server.addr).unwrap();
    sse.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        sse,
        "GET /mcp HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\n\r\n",
        server.addr
    )
    .unwrap();

    let mut reader = BufReader::new(sse.try_clone().unwrap());
    // Read response head; must announce an event stream.
    let mut head = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    assert!(
        head.to_lowercase().contains("text/event-stream"),
        "GET /mcp did not open an SSE stream:\n{head}"
    );

    // 7. Trigger a notification: load a toolset over a separate POST.
    let (status, _, resp) = server.post_mcp(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "load_toolset", "arguments": {"name": "templates"}}
    }));
    assert_eq!(status, 200);
    assert_ne!(resp["result"]["isError"], json!(true));

    // The SSE stream must carry notifications/tools/list_changed.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_notification = false;
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.contains("notifications/tools/list_changed") {
                    saw_notification = true;
                    break;
                }
            }
            Err(_) => break, // read timeout
        }
    }
    assert!(
        saw_notification,
        "tools/list_changed never arrived on the SSE stream"
    );
}

fn ping(id: u32) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#)
}

#[test]
fn origin_is_judged_by_its_parsed_host() {
    let server = HttpServer::spawn();
    for (origin, expected) in [
        ("http://[::1]:3000", 200),
        ("http://127.0.0.1", 200),
        ("http://localhost:3000", 200),
        ("http://localhost.evil.com", 403),
        ("http://127.0.0.1.evil.com", 403),
        ("null", 403),
        ("file://localhost", 403),
    ] {
        let (status, _, _) = server
            .raw_request("POST", "/mcp", &[("Origin", origin)], Some(&ping(11)))
            .unwrap();
        assert_eq!(status, expected, "Origin: {origin}");
    }
}

#[test]
fn a_foreign_host_header_is_forbidden() {
    let server = HttpServer::spawn();
    let port = server.addr.rsplit(':').next().unwrap().to_string();
    for (host, expected) in [
        (format!("evil.example.com:{port}"), 403),
        (format!("localhost.evil.com:{port}"), 403),
        (format!("localhost:{port}"), 200),
        (format!("[::1]:{port}"), 200),
    ] {
        let (status, _, _) = server
            .raw_request("POST", "/mcp", &[("Host", &host)], Some(&ping(12)))
            .unwrap();
        assert_eq!(status, expected, "Host: {host}");
    }
}

fn request_with_token(
    server: &HttpServer,
    method: &str,
    path: &str,
    token: Option<&str>,
) -> (u16, String) {
    let header = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = header
        .as_deref()
        .map(|h| vec![("Authorization", h)])
        .unwrap_or_default();
    let body = (method == "POST").then(|| ping(13));
    let (status, head, _) = server
        .raw_request(method, path, &headers, body.as_deref())
        .unwrap();
    (status, head)
}

#[test]
fn a_configured_token_is_required_on_every_mcp_route() {
    let server = HttpServer::spawn_bound("127.0.0.1", r#","http_token":"s3cret""#, None);

    let (status, head) = request_with_token(&server, "POST", "/mcp", None);
    assert_eq!(status, 401);
    assert!(
        head.to_lowercase().contains("www-authenticate: bearer"),
        "401 must name the scheme:\n{head}"
    );
    assert_eq!(
        request_with_token(&server, "POST", "/mcp", Some("wrong")).0,
        401
    );
    assert_eq!(
        request_with_token(&server, "POST", "/mcp", Some("s3cret")).0,
        200
    );
    assert_eq!(
        request_with_token(&server, "GET", "/mcp/sse", None).0,
        401,
        "the SSE alias is guarded too"
    );

    // Probes stay open; the body carries no tool data.
    let (status, _, body) = server.raw_request("GET", "/health", &[], None).unwrap();
    assert_eq!((status, body.as_str()), (200, "ok"));
}

#[test]
fn the_environment_token_replaces_the_file_token() {
    let server = HttpServer::spawn_bound(
        "127.0.0.1",
        r#","http_token":"from-file""#,
        Some("from-env"),
    );
    assert_eq!(
        request_with_token(&server, "POST", "/mcp", Some("from-file")).0,
        401
    );
    assert_eq!(
        request_with_token(&server, "POST", "/mcp", Some("from-env")).0,
        200
    );
}

/// Run the binary with a config that must be refused, and return its exit
/// status and stderr.
fn refused_startup(extra_config: &str) -> (std::process::ExitStatus, String) {
    let config_json = format!(
        r#"{{"transport":"http","http_address":"0.0.0.0:{}"{extra_config}}}"#,
        free_port()
    );
    let (mut child, _config) = spawn_konnect(&config_json, None, Stdio::piped());
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("konnect kept running with a refused config: {config_json}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (status, stderr)
}

#[test]
fn a_non_loopback_bind_without_opt_in_refuses_to_start() {
    let (status, stderr) = refused_startup(r#","http_token":"t""#);
    assert!(!status.success(), "{status}");
    assert!(stderr.contains("http_allow_remote"), "{stderr}");
}

#[test]
fn a_non_loopback_bind_without_a_token_refuses_to_start() {
    let (status, stderr) = refused_startup(r#","http_allow_remote":true"#);
    assert!(!status.success(), "{status}");
    assert!(stderr.contains("http_token"), "{stderr}");
}

#[test]
fn an_opted_in_remote_bind_serves_with_the_token() {
    let server = HttpServer::spawn_bound(
        "0.0.0.0",
        r#","http_allow_remote":true,"http_token":"s3cret""#,
        None,
    );
    assert_eq!(request_with_token(&server, "POST", "/mcp", None).0, 401);
    assert_eq!(
        request_with_token(&server, "POST", "/mcp", Some("s3cret")).0,
        200
    );
}
