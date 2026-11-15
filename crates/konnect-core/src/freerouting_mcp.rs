//! Local Freerouting MCP orchestration.
//!
//! Konnect deliberately does not reimplement the router. It starts the local
//! Freerouting JAR in its documented headless stdio MCP mode, follows the
//! server's routing state machine, and returns only compact job evidence.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};

#[cfg(test)]
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(test)]
static LAST_CHILD_PID: AtomicU32 = AtomicU32::new(0);

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_STDERR_BYTES: usize = 64 * 1024;
const MAX_SES_BYTES: u64 = 512 * 1024 * 1024;

fn server_arguments() -> &'static [&'static str] {
    &[
        "--api_server.enabled=true",
        "--api_server.authentication.enabled=false",
        "--api_server-endpoints=http://127.0.0.1:37864",
        "--mcp_server.enabled=true",
        "--mcp_server.authentication.enabled=false",
        "--mcp_server-endpoints=http://127.0.0.1:37964",
        "--mcp_server.stdio=true",
        "--gui.enabled=false",
    ]
}

const REQUIRED_TOOLS: &[&str] = &[
    "create_session",
    "enqueue_job",
    "upload_job_input_from_local_file",
    "start_job",
    "get_job_details",
    "download_job_output_to_local_file",
];

#[derive(Debug, Clone)]
pub(crate) struct RouteSettings {
    pub max_passes: Option<u32>,
    pub optimizer_enabled: Option<bool>,
    pub job_timeout_seconds: Option<u64>,
    pub poll_interval: Duration,
    pub overall_timeout: Duration,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteEvidence {
    pub session_id: String,
    pub job_id: String,
    pub final_state: String,
    pub poll_count: u32,
    pub elapsed_seconds: f64,
    pub ses_bytes: u64,
    pub server_protocol_version: String,
    pub diagnostics_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BridgeProbe {
    pub native_mcp_available: bool,
    pub bridge_available: bool,
    pub server_protocol_version: Option<String>,
    pub tool_count: usize,
    pub diagnostics: Option<String>,
    pub error: Option<String>,
}

pub(crate) async fn probe_local(jar: &Path) -> BridgeProbe {
    let mut client = match LocalMcpClient::start(jar).await {
        Ok(client) => client,
        Err(error) => {
            return BridgeProbe {
                native_mcp_available: false,
                bridge_available: false,
                server_protocol_version: None,
                tool_count: 0,
                diagnostics: None,
                error: Some(format!("{error:#}")),
            };
        }
    };
    let server_protocol_version = Some(client.server_protocol_version.clone());
    let contract = async {
        let tools = client.list_tools().await?;
        validate_tool_contracts(&tools)?;
        let missing = REQUIRED_TOOLS
            .iter()
            .filter(|name| !tools.contains_key(**name))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            bail!(
                "Freerouting MCP is missing required tool(s): {}",
                missing.join(", ")
            );
        }
        Ok::<usize, anyhow::Error>(tools.len())
    }
    .await;
    let diagnostics = client.close().await;
    match contract {
        Ok(tool_count) => BridgeProbe {
            native_mcp_available: true,
            bridge_available: true,
            server_protocol_version,
            tool_count,
            diagnostics: (!diagnostics.is_empty()).then_some(diagnostics),
            error: None,
        },
        Err(error) => BridgeProbe {
            native_mcp_available: true,
            bridge_available: false,
            server_protocol_version,
            tool_count: 0,
            diagnostics: (!diagnostics.is_empty()).then_some(diagnostics),
            error: Some(format!("{error:#}")),
        },
    }
}

pub(crate) async fn route_local(
    jar: &Path,
    dsn: &Path,
    ses_output: &Path,
    settings: &RouteSettings,
) -> Result<RouteEvidence> {
    validate_inputs(jar, dsn, ses_output, settings)?;
    if let Some(parent) = ses_output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create SES output directory {}", parent.display()))?;
    }
    let temporary = temporary_output_path(ses_output)?;
    if temporary.exists() {
        bail!(
            "temporary SES output already exists: {}",
            temporary.display()
        );
    }

    let mut client = LocalMcpClient::start(jar).await?;
    let started = Instant::now();
    let route_result = match timeout(
        settings.overall_timeout,
        run_state_machine(&mut client, dsn, &temporary, settings, started),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "Freerouting MCP job exceeded the overall timeout of {} seconds",
            settings.overall_timeout.as_secs()
        )),
    };
    let diagnostics = client.close().await;
    let diagnostics_path = write_diagnostics(ses_output, &diagnostics).await?;

    let result = match route_result {
        Ok(evidence) => Ok(evidence),
        Err(error) => {
            if diagnostics.is_empty() {
                Err(error)
            } else {
                Err(error).context(format!("Freerouting stderr: {diagnostics}"))
            }
        }
    };
    let evidence = match result {
        Ok(evidence) => evidence,
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error);
        }
    };

    let metadata = tokio::fs::metadata(&temporary)
        .await
        .with_context(|| format!("Freerouting did not create {}", temporary.display()))?;
    if metadata.len() == 0 || metadata.len() > MAX_SES_BYTES {
        let _ = tokio::fs::remove_file(&temporary).await;
        bail!(
            "Freerouting SES size {} is outside the supported range 1..={MAX_SES_BYTES}",
            metadata.len()
        );
    }
    let ses_source = tokio::fs::read_to_string(&temporary)
        .await
        .with_context(|| format!("read Freerouting SES {}", temporary.display()))?;
    let tree = konnect_sexp::parse_sexp(&ses_source).context("parse Freerouting SES output")?;
    if tree.head() != Some("session") {
        let _ = tokio::fs::remove_file(&temporary).await;
        bail!("Freerouting output is not a Specctra SES session");
    }
    konnect_sexp::write_new_atomic(ses_output, &ses_source)
        .with_context(|| format!("create SES output {}", ses_output.display()))?;
    tokio::fs::remove_file(&temporary)
        .await
        .with_context(|| format!("remove temporary SES {}", temporary.display()))?;
    Ok(RouteEvidence {
        ses_bytes: metadata.len(),
        diagnostics_path,
        ..evidence
    })
}

async fn write_diagnostics(output: &Path, diagnostics: &str) -> Result<Option<String>> {
    if diagnostics.is_empty() {
        return Ok(None);
    }
    let path = output.with_extension("freerouting.log");
    if path.exists() {
        bail!(
            "Freerouting diagnostics output already exists: {}",
            path.display()
        );
    }
    konnect_sexp::write_new_atomic(&path, diagnostics)
        .with_context(|| format!("create Freerouting diagnostics {}", path.display()))?;
    Ok(Some(path.display().to_string()))
}

fn validate_inputs(
    jar: &Path,
    dsn: &Path,
    ses_output: &Path,
    settings: &RouteSettings,
) -> Result<()> {
    if !jar.is_file() {
        bail!("Freerouting JAR does not exist: {}", jar.display());
    }
    if !dsn.is_file() || !extension_is(dsn, "dsn") {
        bail!("DSN input must be an existing .dsn file: {}", dsn.display());
    }
    if !extension_is(ses_output, "ses") {
        bail!("SES output must have the .ses extension");
    }
    if ses_output.exists() {
        bail!("SES output already exists: {}", ses_output.display());
    }
    if let Some(max_passes) = settings.max_passes {
        if !(1..=100).contains(&max_passes) {
            bail!("max_passes must be between 1 and 100");
        }
    }
    if settings.poll_interval < Duration::from_secs(2)
        || settings.poll_interval > Duration::from_secs(5)
    {
        bail!("poll interval must be between 2 and 5 seconds");
    }
    let minimum_overall_timeout = if cfg!(test) {
        Duration::from_millis(1)
    } else {
        Duration::from_secs(10)
    };
    if settings.overall_timeout < minimum_overall_timeout
        || settings.overall_timeout > Duration::from_secs(86_400)
    {
        bail!("overall timeout must be between 10 and 86400 seconds");
    }
    if settings
        .job_timeout_seconds
        .is_some_and(|seconds| seconds == 0 || seconds > 86_400)
    {
        bail!("job_timeout_seconds must be between 1 and 86400");
    }
    Ok(())
}

async fn run_state_machine(
    client: &mut LocalMcpClient,
    dsn: &Path,
    temporary: &Path,
    settings: &RouteSettings,
    started: Instant,
) -> Result<RouteEvidence> {
    let tools = client.list_tools().await?;
    validate_tool_contracts(&tools)?;
    let missing = REQUIRED_TOOLS
        .iter()
        .filter(|name| !tools.contains_key(**name))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "Freerouting MCP is missing required tool(s): {}",
            missing.join(", ")
        );
    }

    let session = client.call_tool("create_session", json!({})).await?;
    let session_id = required_string(&session, &["sessionId", "session_id", "id"], "session id")?;
    let job = client
        .call_tool(
            "enqueue_job",
            json!({ "body": { "session_id": session_id } }),
        )
        .await?;
    let job_id = required_string(&job, &["jobId", "job_id", "id"], "job id")?;
    client
        .call_tool(
            "upload_job_input_from_local_file",
            json!({ "jobId": job_id, "filePath": absolute_utf8(dsn)? }),
        )
        .await?;

    let mut job_settings = Map::new();
    if let Some(max_passes) = settings.max_passes {
        job_settings.insert("maxPasses".to_string(), json!(max_passes));
    }
    if let Some(enabled) = settings.optimizer_enabled {
        job_settings.insert("optimizer".to_string(), json!({ "enabled": enabled }));
    }
    if let Some(seconds) = settings.job_timeout_seconds {
        job_settings.insert(
            "jobTimeoutString".to_string(),
            Value::String(duration_string(seconds)),
        );
    }
    if !job_settings.is_empty() {
        if !tools.contains_key("update_job_settings") {
            bail!("Freerouting MCP does not expose update_job_settings");
        }
        client
            .call_tool(
                "update_job_settings",
                json!({ "path": { "jobId": job_id }, "body": job_settings }),
            )
            .await?;
    }
    client
        .call_tool("start_job", json!({ "path": { "jobId": job_id } }))
        .await?;

    let mut poll_count = 0u32;
    let final_state = loop {
        let details = client
            .call_tool("get_job_details", json!({ "path": { "jobId": job_id } }))
            .await?;
        poll_count = poll_count.saturating_add(1);
        let state = required_string(&details, &["state", "status"], "job state")?;
        match state.to_ascii_uppercase().as_str() {
            "COMPLETED" => break "COMPLETED".to_string(),
            "FAILED" | "CANCELLED" | "CANCELED" => {
                bail!("Freerouting job {job_id} ended in state {state}")
            }
            _ => tokio::time::sleep(settings.poll_interval).await,
        }
    };

    client
        .call_tool(
            "download_job_output_to_local_file",
            json!({ "jobId": job_id, "filePath": absolute_utf8(temporary)? }),
        )
        .await?;
    Ok(RouteEvidence {
        session_id,
        job_id,
        final_state,
        poll_count,
        elapsed_seconds: started.elapsed().as_secs_f64(),
        ses_bytes: 0,
        server_protocol_version: client.server_protocol_version.clone(),
        diagnostics_path: None,
    })
}

fn validate_tool_contracts(tools: &BTreeMap<String, Value>) -> Result<()> {
    for (tool, paths) in [
        ("enqueue_job", &[(&["body", "session_id"][..], false)][..]),
        (
            "upload_job_input_from_local_file",
            &[(&["jobId"][..], true), (&["filePath"][..], true)][..],
        ),
        ("start_job", &[(&["path", "jobId"][..], true)][..]),
        ("get_job_details", &[(&["path", "jobId"][..], true)][..]),
        (
            "download_job_output_to_local_file",
            &[(&["jobId"][..], true), (&["filePath"][..], true)][..],
        ),
    ] {
        let schema = tools
            .get(tool)
            .with_context(|| format!("Freerouting MCP is missing required tool '{tool}'"))?;
        for (path, leaf_must_be_required) in paths {
            require_schema_path(tool, schema, path, *leaf_must_be_required)?;
        }
    }
    Ok(())
}

fn require_schema_path(
    tool: &str,
    schema: &Value,
    path: &[&str],
    leaf_must_be_required: bool,
) -> Result<()> {
    let mut current = schema;
    for (index, key) in path.iter().enumerate() {
        if current.get("type").and_then(Value::as_str) != Some("object") {
            bail!(
                "Freerouting MCP tool '{tool}' has an incompatible input schema at '{}'",
                path.join(".")
            );
        }
        let required = current
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(key)));
        if !required && (index + 1 < path.len() || leaf_must_be_required) {
            bail!(
                "Freerouting MCP tool '{tool}' does not require argument '{}'",
                path.join(".")
            );
        }
        current = current
            .get("properties")
            .and_then(|properties| properties.get(*key))
            .with_context(|| {
                format!(
                    "Freerouting MCP tool '{tool}' has no argument '{}'",
                    path.join(".")
                )
            })?;
    }
    Ok(())
}

struct LocalMcpClient {
    child: Option<Child>,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr_task: Option<JoinHandle<String>>,
    next_id: u64,
    server_protocol_version: String,
}

impl LocalMcpClient {
    async fn start(jar: &Path) -> Result<Self> {
        let mut client = Self::spawn(jar)?;
        if let Err(error) = client.initialize().await {
            let diagnostics = client.close().await;
            return if diagnostics.is_empty() {
                Err(error)
            } else {
                Err(error).context(format!("Freerouting stderr: {diagnostics}"))
            };
        }
        Ok(client)
    }

    fn spawn(jar: &Path) -> Result<Self> {
        let mut command = Command::new("java");
        command
            .arg("-jar")
            .arg(jar)
            .args(server_arguments())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .context("start local Freerouting MCP server")?;
        #[cfg(test)]
        if let Some(pid) = child.id() {
            LAST_CHILD_PID.store(pid, Ordering::SeqCst);
        }
        let stdin = child.stdin.take().context("Freerouting MCP has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Freerouting MCP has no stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("Freerouting MCP has no stderr")?;
        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut bytes = Vec::new();
            let _ = reader
                .take(MAX_STDERR_BYTES as u64)
                .read_to_end(&mut bytes)
                .await;
            String::from_utf8_lossy(&bytes).trim().to_string()
        });
        Ok(Self {
            child: Some(child),
            stdin,
            stdout: BufReader::new(stdout).lines(),
            stderr_task: Some(stderr_task),
            next_id: 1,
            server_protocol_version: String::new(),
        })
    }

    async fn initialize(&mut self) -> Result<()> {
        let initialize = timeout(
            STARTUP_TIMEOUT,
            self.request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "Konnect", "version": env!("CARGO_PKG_VERSION") }
                }),
                STARTUP_TIMEOUT,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Freerouting MCP initialization timed out"))??;
        self.server_protocol_version = find_string(&initialize, &["protocolVersion"])
            .unwrap_or_else(|| MCP_PROTOCOL_VERSION.to_string());
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    async fn list_tools(&mut self) -> Result<BTreeMap<String, Value>> {
        let result = self
            .request("tools/list", json!({}), REQUEST_TIMEOUT)
            .await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .context("Freerouting tools/list returned no tools array")?;
        tools
            .iter()
            .map(|tool| {
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .context("Freerouting tools/list contains an unnamed tool")?;
                let schema = tool
                    .get("inputSchema")
                    .cloned()
                    .context("Freerouting tools/list contains a tool without inputSchema")?;
                Ok((name, schema))
            })
            .collect()
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                REQUEST_TIMEOUT,
            )
            .await
            .with_context(|| format!("call Freerouting MCP tool '{name}'"))?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            bail!(
                "Freerouting MCP tool '{name}' returned an error: {}",
                compact(&result)
            );
        }
        Ok(expand_text_content(result))
    }

    async fn request(&mut self, method: &str, params: Value, limit: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.write_json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;
        let deadline = Instant::now() + limit;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("Freerouting MCP request '{method}' timed out");
            }
            let line = timeout(remaining, self.stdout.next_line())
                .await
                .map_err(|_| anyhow::anyhow!("Freerouting MCP request '{method}' timed out"))??
                .context("Freerouting MCP closed stdout before responding")?;
            if line.trim().is_empty() {
                continue;
            }
            let response: Value = serde_json::from_str(&line)
                .with_context(|| format!("Freerouting MCP emitted non-JSON stdout: {line}"))?;
            if response.get("id") != Some(&json!(id)) {
                if response.get("method").is_some() {
                    continue;
                }
                bail!("Freerouting MCP returned an unexpected response id");
            }
            if let Some(error) = response.get("error") {
                bail!(
                    "Freerouting MCP request '{method}' failed: {}",
                    compact(error)
                );
            }
            return response
                .get("result")
                .cloned()
                .context("Freerouting MCP response has no result");
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_json(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    async fn write_json(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .context("write Freerouting MCP stdin")?;
        self.stdin
            .flush()
            .await
            .context("flush Freerouting MCP stdin")
    }

    async fn close(mut self) -> String {
        let _ = self.stdin.shutdown().await;
        if let Some(mut child) = self.child.take() {
            if timeout(SHUTDOWN_TIMEOUT, child.wait()).await.is_err() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
        match self.stderr_task.take() {
            Some(task) => task.await.unwrap_or_default(),
            None => String::new(),
        }
    }
}

impl Drop for LocalMcpClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}

fn expand_text_content(mut result: Value) -> Value {
    let parsed = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .collect::<Vec<_>>();
    if let Some(object) = result.as_object_mut() {
        object.insert("parsedTextContent".to_string(), Value::Array(parsed));
    }
    result
}

fn required_string(value: &Value, keys: &[&str], label: &str) -> Result<String> {
    find_string(value, keys).with_context(|| {
        format!(
            "Freerouting MCP response has no {label}: {}",
            compact(value)
        )
    })
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if keys
                    .iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
                {
                    if let Some(value) = value.as_str() {
                        return Some(value.to_string());
                    }
                }
            }
            object.values().find_map(|value| find_string(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, keys)),
        _ => None,
    }
}

fn absolute_utf8(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    absolute
        .to_str()
        .map(str::to_string)
        .with_context(|| format!("path is not UTF-8: {}", absolute.display()))
}

fn temporary_output_path(output: &Path) -> Result<PathBuf> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .context("SES output has no UTF-8 filename")?;
    Ok(parent.join(format!(".{name}.konnect-{}.tmp.ses", uuid::Uuid::new_v4())))
}

fn duration_string(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn extension_is(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 2_000 {
        let truncated: String = text.chars().take(2_000).collect();
        format!("{truncated}...")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use konnect_ipc::{IpcEffectiveRoutingRules, IpcRoutingRules};

    #[cfg(windows)]
    fn process_is_running(pid: u32) -> bool {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .expect("run tasklist");
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    }

    #[cfg(not(windows))]
    fn process_is_running(pid: u32) -> bool {
        std::process::Command::new("sh")
            .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
            .status()
            .expect("probe process")
            .success()
    }

    async fn wait_for_process_exit(pid: u32) -> bool {
        for _ in 0..50 {
            if !process_is_running(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    fn routing_rules() -> IpcEffectiveRoutingRules {
        ["GND", "VCC"]
            .into_iter()
            .map(|net| {
                (
                    net.to_string(),
                    IpcRoutingRules {
                        class_name: "Default".to_string(),
                        constituents: vec!["Default".to_string()],
                        track_width_mm: Some(0.25),
                        clearance_mm: Some(0.2),
                        via_diameter_mm: Some(0.6),
                        via_drill_mm: Some(0.3),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn nested_and_text_wrapped_identifiers_are_found() {
        let value = expand_text_content(json!({
            "content": [{ "type": "text", "text": "{\"data\":{\"sessionId\":\"abc\"}}" }]
        }));
        assert_eq!(find_string(&value, &["sessionId"]), Some("abc".to_string()));
    }

    #[test]
    fn duration_settings_use_documented_shape() {
        assert_eq!(duration_string(5 * 60), "00:05:00");
        assert_eq!(duration_string(25 * 60 * 60), "25:00:00");
    }

    #[test]
    fn output_path_is_unique_and_stays_beside_destination() {
        let output = Path::new("build/board.ses");
        let first = temporary_output_path(output).unwrap();
        let second = temporary_output_path(output).unwrap();
        assert_eq!(first.parent(), output.parent());
        assert_ne!(first, second);
        assert!(extension_is(&first, "ses"));
    }

    #[test]
    fn unauthenticated_servers_are_forced_to_loopback() {
        let arguments = server_arguments();
        assert!(arguments.contains(&"--api_server.authentication.enabled=false"));
        assert!(arguments.contains(&"--api_server-endpoints=http://127.0.0.1:37864"));
        assert!(arguments.contains(&"--mcp_server.authentication.enabled=false"));
        assert!(arguments.contains(&"--mcp_server-endpoints=http://127.0.0.1:37964"));
        assert!(!arguments
            .iter()
            .any(|argument| argument.contains("0.0.0.0")));
    }

    #[test]
    fn required_tool_schema_drift_is_refused_before_routing() {
        let object = |properties: Value, required: Value| json!({ "type": "object", "properties": properties, "required": required });
        let mut tools = BTreeMap::new();
        tools.insert("enqueue_job".into(), object(json!({"body": object(json!({"session_id": {"type":"string"}}), json!(["session_id"]))}), json!(["body"])));
        tools.insert(
            "upload_job_input_from_local_file".into(),
            object(
                json!({"jobId":{"type":"string"},"filePath":{"type":"string"}}),
                json!(["jobId", "filePath"]),
            ),
        );
        tools.insert(
            "start_job".into(),
            object(
                json!({"path": object(json!({"jobId":{"type":"string"}}), json!(["jobId"]))}),
                json!(["path"]),
            ),
        );
        tools.insert("get_job_details".into(), tools["start_job"].clone());
        tools.insert(
            "download_job_output_to_local_file".into(),
            tools["upload_job_input_from_local_file"].clone(),
        );
        validate_tool_contracts(&tools).unwrap();

        tools.get_mut("start_job").unwrap()["properties"]["path"]["required"] = json!([]);
        let error = validate_tool_contracts(&tools).unwrap_err().to_string();
        assert!(
            error.contains("start_job") && error.contains("path.jobId"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn stderr_diagnostics_are_preserved_beside_the_requested_output() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("board.ses");
        let path = write_diagnostics(&output, "router failed at pass 3")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "router failed at pass 3"
        );
    }

    /// Optional local parity check for Freerouting's actual stdio MCP state
    /// machine. CI does not install Java or Freerouting.
    #[tokio::test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    async fn native_mcp_routes_the_export_fixture() {
        let jar = PathBuf::from(std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR"));
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&board, source).unwrap();
        let export = crate::specctra::export_dsn(&board, source, &routing_rules()).unwrap();
        std::fs::write(&dsn, export.dsn).unwrap();

        let evidence = route_local(
            &jar,
            &dsn,
            &ses,
            &RouteSettings {
                max_passes: Some(2),
                optimizer_enabled: Some(false),
                job_timeout_seconds: Some(300),
                poll_interval: Duration::from_secs(2),
                overall_timeout: Duration::from_secs(300),
            },
        )
        .await
        .unwrap();
        assert_eq!(evidence.final_state, "COMPLETED");
        assert!(evidence.ses_bytes > 0);
        assert!(ses.is_file());
    }

    #[tokio::test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    async fn overall_timeout_closes_child_and_removes_partial_output() {
        let jar = PathBuf::from(std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR"));
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&board, source).unwrap();
        let export = crate::specctra::export_dsn(&board, source, &routing_rules()).unwrap();
        std::fs::write(&dsn, export.dsn).unwrap();

        let error = format!(
            "{:#}",
            route_local(
                &jar,
                &dsn,
                &ses,
                &RouteSettings {
                    max_passes: Some(100),
                    optimizer_enabled: Some(true),
                    job_timeout_seconds: Some(300),
                    poll_interval: Duration::from_secs(5),
                    overall_timeout: Duration::from_millis(100),
                },
            )
            .await
            .unwrap_err()
        );
        assert!(error.contains("overall timeout"), "{error}");
        let pid = LAST_CHILD_PID.load(Ordering::SeqCst);
        assert!(
            wait_for_process_exit(pid).await,
            "Freerouting child {pid} survived timeout"
        );
        assert!(!ses.exists());
        assert!(
            std::fs::read_dir(temp.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.ses")),
            "timeout left a temporary SES"
        );
    }

    #[tokio::test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    async fn cancelling_route_reaps_the_owned_child() {
        let jar = PathBuf::from(std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR"));
        let source = include_str!("../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&board, source).unwrap();
        let export = crate::specctra::export_dsn(&board, source, &routing_rules()).unwrap();
        std::fs::write(&dsn, export.dsn).unwrap();

        LAST_CHILD_PID.store(0, Ordering::SeqCst);
        let task = tokio::spawn(async move {
            route_local(
                &jar,
                &dsn,
                &ses,
                &RouteSettings {
                    max_passes: Some(100),
                    optimizer_enabled: Some(true),
                    job_timeout_seconds: Some(300),
                    poll_interval: Duration::from_secs(5),
                    overall_timeout: Duration::from_secs(300),
                },
            )
            .await
        });
        let pid = loop {
            let pid = LAST_CHILD_PID.load(Ordering::SeqCst);
            if pid != 0 {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        task.abort();
        let _ = task.await;
        assert!(
            wait_for_process_exit(pid).await,
            "Freerouting child {pid} survived cancellation"
        );
    }

    fn coverage_settings() -> RouteSettings {
        RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            poll_interval: Duration::from_secs(2),
            overall_timeout: Duration::from_secs(10),
        }
    }

    #[test]
    fn routing_inputs_are_validated_before_starting_a_process_or_creating_output() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("router.jar");
        let dsn = dir.path().join("board.DSN");
        let ses = dir.path().join("new.ses");
        std::fs::write(&jar, "fixture jar is never executed").unwrap();
        std::fs::write(&dsn, "(pcb board)").unwrap();
        validate_inputs(&jar, &dsn, &ses, &coverage_settings()).unwrap();
        for (settings, expected) in [
            (
                RouteSettings {
                    max_passes: Some(0),
                    ..coverage_settings()
                },
                "max_passes",
            ),
            (
                RouteSettings {
                    max_passes: Some(101),
                    ..coverage_settings()
                },
                "max_passes",
            ),
            (
                RouteSettings {
                    poll_interval: Duration::from_secs(1),
                    ..coverage_settings()
                },
                "poll interval",
            ),
            (
                RouteSettings {
                    poll_interval: Duration::from_secs(6),
                    ..coverage_settings()
                },
                "poll interval",
            ),
            (
                RouteSettings {
                    overall_timeout: Duration::ZERO,
                    ..coverage_settings()
                },
                "overall timeout",
            ),
            (
                RouteSettings {
                    overall_timeout: Duration::from_secs(86401),
                    ..coverage_settings()
                },
                "overall timeout",
            ),
            (
                RouteSettings {
                    job_timeout_seconds: Some(0),
                    ..coverage_settings()
                },
                "job_timeout_seconds",
            ),
            (
                RouteSettings {
                    job_timeout_seconds: Some(86401),
                    ..coverage_settings()
                },
                "job_timeout_seconds",
            ),
        ] {
            let error = validate_inputs(&jar, &dsn, &ses, &settings).unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert!(!ses.exists());
        }
        for settings in [
            RouteSettings {
                max_passes: Some(1),
                job_timeout_seconds: Some(1),
                ..coverage_settings()
            },
            RouteSettings {
                max_passes: Some(100),
                job_timeout_seconds: Some(86400),
                poll_interval: Duration::from_secs(5),
                overall_timeout: Duration::from_secs(86400),
                ..coverage_settings()
            },
        ] {
            validate_inputs(&jar, &dsn, &ses, &settings).unwrap();
        }
        assert!(validate_inputs(
            &dir.path().join("missing.jar"),
            &dsn,
            &ses,
            &coverage_settings()
        )
        .unwrap_err()
        .to_string()
        .contains("JAR does not exist"));
        assert!(validate_inputs(
            &jar,
            &dir.path().join("missing.dsn"),
            &ses,
            &coverage_settings()
        )
        .unwrap_err()
        .to_string()
        .contains("DSN input"));
        assert!(validate_inputs(&jar, &jar, &ses, &coverage_settings())
            .unwrap_err()
            .to_string()
            .contains("DSN input"));
        assert!(validate_inputs(
            &jar,
            &dsn,
            &dir.path().join("output.txt"),
            &coverage_settings()
        )
        .unwrap_err()
        .to_string()
        .contains(".ses extension"));
        std::fs::write(&ses, "existing board routes").unwrap();
        assert!(validate_inputs(&jar, &dsn, &ses, &coverage_settings())
            .unwrap_err()
            .to_string()
            .contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(ses).unwrap(),
            "existing board routes"
        );
    }

    #[tokio::test]
    async fn routing_diagnostics_do_not_clobber_existing_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("board.ses");
        assert!(write_diagnostics(&output, "").await.unwrap().is_none());
        assert!(!output.with_extension("freerouting.log").exists());
        let log = output.with_extension("freerouting.log");
        std::fs::write(&log, "previous run evidence").unwrap();
        let error = write_diagnostics(&output, "new run failure")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(log).unwrap(),
            "previous run evidence"
        );
    }

    // A local stdio fixture exercises the actual JSON-RPC framing without Java
    // or a global PATH override. Responses are paired with recorded requests;
    // the initialize notification consumes no response, as MCP requires.
    #[cfg(unix)]
    async fn scripted_client(trace: &Path, replies: &[String]) -> LocalMcpClient {
        let script = r#"trace=$1; shift
for reply do
  while IFS= read -r request; do
    printf '%s\n' "$request" >> "$trace"
    case "$request" in
      *'"method":"notifications/initialized"'*) continue ;;
      *) printf '%s\n' "$reply"; break ;;
    esac
  done
done
cat >/dev/null"#;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .arg("fixture")
            .arg(trace)
            .arg(json!({"id":1,"result":{"protocolVersion":"2024-11-05"}}).to_string())
            .args(replies)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut client = LocalMcpClient {
            child: Some(child),
            stdin,
            stdout,
            stderr_task: None,
            next_id: 1,
            server_protocol_version: String::new(),
        };
        client.initialize().await.unwrap();
        client
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_protocol_refuses_unmatched_errors_and_malformed_tool_results() {
        let dir = tempfile::tempdir().unwrap();
        for (reply, expected) in [
            ("not-json".to_string(), "non-JSON stdout"),
            (
                json!({"id":99,"result":{}}).to_string(),
                "unexpected response id",
            ),
            (
                json!({"id":2,"error":{"message":"router rejected"}}).to_string(),
                "router rejected",
            ),
            (json!({"id":2}).to_string(), "no result"),
        ] {
            let mut client = scripted_client(&dir.path().join("protocol.log"), &[reply]).await;
            let error = client
                .request("probe", json!({}), Duration::from_secs(1))
                .await
                .unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
            client.close().await;
        }
        for (result, expected) in [
            (json!({}), "no tools array"),
            (json!({"tools":[{"inputSchema":{}}]}), "unnamed tool"),
            (json!({"tools":[{"name":"route"}]}), "without inputSchema"),
        ] {
            let reply = json!({"id":2,"result":result}).to_string();
            let mut client = scripted_client(&dir.path().join("tools.log"), &[reply]).await;
            assert!(client
                .list_tools()
                .await
                .unwrap_err()
                .to_string()
                .contains(expected));
            client.close().await;
        }
        let reply = json!({"id":2,"result":{"isError":true,"content":[{"type":"text","text":"invalid input"}]}}).to_string();
        let mut client = scripted_client(&dir.path().join("error.log"), &[reply]).await;
        assert!(client
            .call_tool("route", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("returned an error"));
        client.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_notifications_and_text_results_keep_the_matching_response() {
        let dir = tempfile::tempdir().unwrap();
        let reply = format!(
            "\n{}\n{}",
            json!({"method":"progress","params":{"percent":10}}),
            json!({"id":2,"result":{"content":[{"type":"text","text":"{\"sessionId\":\"fixture-session\"}"}]}})
        );
        let trace = dir.path().join("trace.log");
        let mut client = scripted_client(&trace, &[reply]).await;
        let result = client.call_tool("create_session", json!({})).await.unwrap();
        assert_eq!(
            required_string(&result, &["sessionId"], "session id").unwrap(),
            "fixture-session"
        );
        assert_eq!(client.server_protocol_version, "2024-11-05");
        client.close().await;
        let requests: Vec<Value> = std::fs::read_to_string(trace)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(requests[0]["method"], "initialize");
        assert_eq!(requests[1]["method"], "notifications/initialized");
        assert!(requests[1].get("id").is_none());
        assert_eq!(requests[2]["params"]["name"], "create_session");
    }

    #[cfg(unix)]
    fn coverage_tool_list(include_settings: bool) -> Value {
        let flat = json!({"type":"object","properties":{"jobId":{"type":"string"},"filePath":{"type":"string"}},"required":["jobId","filePath"]});
        let path = json!({"type":"object","properties":{"path":{"type":"object","properties":{"jobId":{"type":"string"}},"required":["jobId"]}},"required":["path"]});
        let body = json!({"type":"object","properties":{"body":{"type":"object","properties":{"session_id":{"type":"string"}},"required":["session_id"]}},"required":["body"]});
        let mut tools = vec![
            json!({"name":"create_session","inputSchema":{"type":"object"}}),
            json!({"name":"enqueue_job","inputSchema":body}),
            json!({"name":"upload_job_input_from_local_file","inputSchema":flat}),
            json!({"name":"start_job","inputSchema":path}),
            json!({"name":"get_job_details","inputSchema":path}),
            json!({"name":"download_job_output_to_local_file","inputSchema":flat}),
        ];
        if include_settings {
            tools.push(json!({"name":"update_job_settings","inputSchema":{"type":"object"}}));
        }
        json!({"tools":tools})
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_routing_state_machine_sends_settings_and_downloads_only_completed_jobs() {
        let dir = tempfile::tempdir().unwrap();
        for (configure, state) in [
            (false, "COMPLETED"),
            (true, "COMPLETED"),
            (false, "FAILED"),
            (false, "CANCELLED"),
            (false, "CANCELED"),
        ] {
            let trace = dir.path().join(format!("{configure}-{state}.log"));
            let mut results = vec![
                coverage_tool_list(configure),
                json!({"sessionId":"session-fixture"}),
                json!({"jobId":"job-fixture"}),
                json!({}),
            ];
            let mut settings = coverage_settings();
            if configure {
                settings.max_passes = Some(2);
                settings.optimizer_enabled = Some(false);
                settings.job_timeout_seconds = Some(300);
                results.push(json!({}));
            }
            results.extend([json!({}), json!({"state":state}), json!({})]);
            let replies = results
                .into_iter()
                .enumerate()
                .map(|(index, result)| json!({"id":index + 2,"result":result}).to_string())
                .collect::<Vec<_>>();
            let mut client = scripted_client(&trace, &replies).await;
            let dsn = dir.path().join("input.dsn");
            let temporary = dir.path().join("output.tmp.ses");
            let result =
                run_state_machine(&mut client, &dsn, &temporary, &settings, Instant::now()).await;
            if state == "COMPLETED" {
                let evidence = result.unwrap();
                assert_eq!(evidence.session_id, "session-fixture");
                assert_eq!(evidence.job_id, "job-fixture");
                assert_eq!(evidence.final_state, "COMPLETED");
                assert_eq!(evidence.poll_count, 1);
            } else {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains(&format!("ended in state {state}")));
            }
            client.close().await;
            let requests = std::fs::read_to_string(&trace)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            let calls = requests
                .iter()
                .filter(|request| request["method"] == "tools/call")
                .collect::<Vec<_>>();
            assert_eq!(calls[0]["params"]["name"], "create_session");
            assert_eq!(
                calls[1]["params"]["arguments"]["body"]["session_id"],
                "session-fixture"
            );
            assert_eq!(calls[2]["params"]["arguments"]["jobId"], "job-fixture");
            let update = calls
                .iter()
                .find(|request| request["params"]["name"] == "update_job_settings");
            assert_eq!(update.is_some(), configure);
            if let Some(update) = update {
                assert_eq!(
                    update["params"]["arguments"]["body"],
                    json!({"maxPasses":2,"optimizer":{"enabled":false},"jobTimeoutString":"00:05:00"})
                );
            }
            assert_eq!(calls.iter().any(|request| request["params"]["name"] == "download_job_output_to_local_file"),state == "COMPLETED");
            assert!(
                !temporary.exists(),
                "the stdio fixture must not impersonate real router output"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn routing_protocol_refusals_stop_before_creating_or_downloading_routes() {
        let dir = tempfile::tempdir().unwrap();
        let mut missing_session_tool = coverage_tool_list(false);
        missing_session_tool["tools"]
            .as_array_mut()
            .unwrap()
            .retain(|tool| tool["name"] != "create_session");
        let mut bad_type = coverage_tool_list(false);
        bad_type["tools"][1]["inputSchema"]["type"] = json!("string");
        let mut missing_path = coverage_tool_list(false);
        missing_path["tools"][1]["inputSchema"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("body");
        let mut missing_required = coverage_tool_list(false);
        missing_required["tools"][1]["inputSchema"]["required"] = json!([]);
        for (index, (results, configure, expected)) in [
            (
                vec![missing_session_tool],
                false,
                "missing required tool(s): create_session",
            ),
            (vec![bad_type], false, "incompatible input schema"),
            (vec![missing_path], false, "has no argument"),
            (vec![missing_required], false, "does not require argument"),
            (
                vec![coverage_tool_list(false), json!({})],
                false,
                "no session id",
            ),
            (
                vec![
                    coverage_tool_list(false),
                    json!({"sessionId":"fixture-session"}),
                    json!({}),
                ],
                false,
                "no job id",
            ),
            (
                vec![
                    coverage_tool_list(false),
                    json!({"sessionId":"fixture-session"}),
                    json!({"jobId":"fixture-job"}),
                    json!({}),
                ],
                true,
                "does not expose update_job_settings",
            ),
            (
                vec![
                    coverage_tool_list(false),
                    json!({"sessionId":"fixture-session"}),
                    json!({"jobId":"fixture-job"}),
                    json!({}),
                    json!({}),
                    json!({}),
                ],
                false,
                "no job state",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let replies = results
                .into_iter()
                .enumerate()
                .map(|(index, result)| json!({"id":index + 2,"result":result}).to_string())
                .collect::<Vec<_>>();
            let trace = dir.path().join(format!("refused-{index}.log"));
            let mut client = scripted_client(&trace, &replies).await;
            let mut settings = coverage_settings();
            if configure {
                settings.max_passes = Some(2);
            }
            let output = dir.path().join("output.ses");
            let error = run_state_machine(
                &mut client,
                &dir.path().join("input.dsn"),
                &output,
                &settings,
                Instant::now(),
            )
            .await
            .unwrap_err();
            client.close().await;
            assert!(
                format!("{error:#}").contains(expected),
                "case {index}: {error:#}"
            );
            let requests = std::fs::read_to_string(trace).unwrap();
            assert!(!requests.contains("download_job_output_to_local_file"));
            assert!(!output.exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn routing_polls_a_waiting_job_until_completion_and_preserves_the_job_identity() {
        let dir = tempfile::tempdir().unwrap();
        let trace = dir.path().join("poll.log");
        let results = [
            coverage_tool_list(false),
            json!({"sessionId":"fixture-session"}),
            json!({"jobId":"fixture-job"}),
            json!({}),
            json!({}),
            json!({"state":"RUNNING"}),
            json!({"status":"completed"}),
            json!({}),
        ];
        let replies = results
            .into_iter()
            .enumerate()
            .map(|(index, result)| json!({"id":index + 2,"result":result}).to_string())
            .collect::<Vec<_>>();
        let mut client = scripted_client(&trace, &replies).await;
        let settings = RouteSettings {
            poll_interval: Duration::from_millis(1),
            ..coverage_settings()
        };
        let evidence = run_state_machine(
            &mut client,
            &dir.path().join("input.dsn"),
            &dir.path().join("output.ses"),
            &settings,
            Instant::now(),
        )
        .await
        .unwrap();
        client.close().await;
        assert_eq!(evidence.poll_count, 2);
        assert_eq!(evidence.job_id, "fixture-job");
        let requests = std::fs::read_to_string(trace).unwrap();
        assert_eq!(requests.matches("get_job_details").count(), 2);
        assert_eq!(
            requests
                .matches("download_job_output_to_local_file")
                .count(),
            1
        );
    }
}
