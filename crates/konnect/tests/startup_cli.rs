//! Guidance-install invariants for real CLI process startup.
//!
//! Windows is intentionally excluded: `dirs::home_dir()` uses
//! `SHGetKnownFolderPath`, so overriding `HOME`/`USERPROFILE` would still risk
//! modifying the test runner's real client configuration.

#![cfg(not(target_os = "windows"))]

use serde_json::json;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn konnect(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_konnect"));
    command.env("HOME", home);
    command.env("XDG_CONFIG_HOME", home.join(".config"));
    command.env("KONNECT_STATE_DIR", home.join(".konnect-state"));
    command
}

fn guidance_snapshot(home: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fn visit(root: &Path, dir: &Path, entries: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
        let mut children: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                entries.push((relative, None));
                visit(root, &path, entries);
            } else {
                entries.push((relative, Some(fs::read(path).unwrap())));
            }
        }
    }

    let mut entries = Vec::new();
    for name in [".claude", ".agents"] {
        let path = home.join(name);
        if path.exists() {
            entries.push((PathBuf::from(name), None));
            visit(home, &path, &mut entries);
        }
    }
    for name in [
        ".konnect/.installed",
        ".konnect/.installed-claude",
        ".konnect/.installed-codex",
    ] {
        let path = home.join(name);
        if path.exists() {
            entries.push((PathBuf::from(name), Some(fs::read(path).unwrap())));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

fn start_and_initialize_server(home: &Path, client: &str) {
    let mut child = konnect(home)
        .args(["--client", client])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "startup-test", "version": "0"}
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut response)
        .unwrap();
    assert!(response.contains("\"serverInfo\""), "{response}");

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn mcp_start_does_not_install_into_a_clean_home() {
    for client in ["claude", "codex"] {
        let temp = tempfile::tempdir().unwrap();
        let before = guidance_snapshot(temp.path());

        start_and_initialize_server(temp.path(), client);

        assert_eq!(guidance_snapshot(temp.path()), before, "client: {client}");
    }
}

#[test]
fn mcp_start_does_not_reverse_an_explicit_uninstall() {
    for client in ["claude", "codex"] {
        let temp = tempfile::tempdir().unwrap();

        assert!(konnect(temp.path())
            .args(["init", "--client", client])
            .status()
            .unwrap()
            .success());
        assert!(konnect(temp.path())
            .args(["uninstall", "--client", client])
            .status()
            .unwrap()
            .success());

        let before = guidance_snapshot(temp.path());
        start_and_initialize_server(temp.path(), client);
        assert_eq!(guidance_snapshot(temp.path()), before, "client: {client}");
    }
}

#[test]
fn cwd_configuration_is_not_loaded_implicitly() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    for (name, decoy) in [("konnect.toml", "["), ("settings.json", "[")] {
        let path = project.path().join(name);
        fs::write(&path, decoy).unwrap();
        let output = konnect(home.path())
            .current_dir(project.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "cwd candidate {name} interrupted startup: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let explicit = konnect(home.path())
            .current_dir(project.path())
            .args(["--config", name])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            !explicit.status.success(),
            "explicit configuration was bypassed"
        );
        fs::remove_file(path).unwrap();
    }
}

fn terminal() -> (File, File) {
    let (mut master, mut slave) = (-1, -1);
    // openpty initializes both descriptors on success. Keeping the master
    // open prevents a terminal hangup while the child uses the slave.
    let status = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(status, 0, "{}", std::io::Error::last_os_error());
    // Transfer ownership before setting close-on-exec so any panic closes both FDs.
    let files = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
    for fd in [master, slave] {
        // These descriptors are open and owned by files.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    files
}

#[test]
fn terminal_start_with_http_config_serves_health_without_installing() {
    let home = tempfile::tempdir().unwrap();
    let address = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let config = home.path().join("http.toml");
    fs::write(
        &config,
        format!("transport = \"http\"\nhttp_address = \"{address}\"\n"),
    )
    .unwrap();

    let (_master, slave) = terminal();
    let mut child = konnect(home.path())
        .args(["--config", config.to_str().unwrap()])
        .stdin(slave)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut healthy = false;
    while Instant::now() < deadline && child.try_wait().unwrap().is_none() {
        if let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
            let mut reply = String::new();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            healthy = stream
                .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .and_then(|_| stream.read_to_string(&mut reply))
                .is_ok()
                && reply.starts_with("HTTP/1.1 200 ")
                && reply.ends_with("\r\n\r\nok");
            if healthy {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    assert!(guidance_snapshot(home.path()).is_empty());
    assert!(healthy, "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn terminal_start_with_default_stdio_prints_help_without_installing() {
    let home = tempfile::tempdir().unwrap();
    let (_master, slave) = terminal();
    let mut child = konnect(home.path())
        .stdin(slave)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "default terminal startup did not exit successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("shows help on a terminal"));
    assert!(guidance_snapshot(home.path()).is_empty());
}

#[test]
fn claude_init_from_npx_cache_warns_about_hook_path() {
    let home = tempfile::tempdir().unwrap();
    let bin = home
        .path()
        .join("_npx/test/node_modules/@konnect-xai/konnect-platform/bin");
    fs::create_dir_all(&bin).unwrap();
    let executable = bin.join("konnect");
    let source = env!("CARGO_BIN_EXE_konnect");
    fs::hard_link(source, &executable)
        .or_else(|_| fs::copy(source, &executable).map(|_| ()))
        .unwrap();

    let output = Command::new(&executable)
        .args(["init", "--client", "claude"])
        .env("HOME", home.path())
        .env("KONNECT_STATE_DIR", home.path().join(".konnect-state"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Claude hooks use this npx cache binary")
    );
}
