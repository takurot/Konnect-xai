//! Per-call observability: ring buffer of recent calls, per-tool counters,
//! and optional JSONL log file at `<konnect config dir>/logs/calls.jsonl`.
//!
//! Wired into `McpHandler::execute_tool` — every tool invocation (meta-tool or
//! domain tool) lands a `CallRecord` via `CallObserver::record`. The LLM can
//! introspect via the `get_recent_calls` and `server_stats` meta-tools.
//!
//! Goals:
//! - Optional disk I/O runs on a blocking worker outside the async data locks.
//! - Failure isolation: if the log file can't be opened or written, the tool
//!   call still succeeds; we `tracing::warn!` and move on.
//! - Bounded memory: ring buffer caps at `MAX_RECENT_CALLS`.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::warn;

/// Keep the last N calls in memory for `get_recent_calls`.
pub const MAX_RECENT_CALLS: usize = 100;
const MAX_CALL_LOG_BYTES: u64 = 5 * 1024 * 1024;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Outcome of a single tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    /// Tool ran to completion and reported success.
    Ok,
    /// Some requested items or checks completed and others did not.
    Partial,
    /// Persistence may have happened, but readback could not establish it.
    Uncertain,
    /// Tool ran to completion but reported `is_error: true`, or the handler
    /// returned an `anyhow::Error`.
    Error,
    /// Tool not found or in an unloaded toolset.
    NotFound,
}

impl CallStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CallStatus::Ok => "ok",
            CallStatus::Partial => "partial",
            CallStatus::Uncertain => "uncertain",
            CallStatus::Error => "error",
            CallStatus::NotFound => "not_found",
        }
    }
}

/// One recorded tool invocation. Keep fields compact — this is serialized to
/// JSONL on every call.
#[derive(Debug, Clone, Serialize)]
pub struct CallRecord {
    pub call_id: String,
    /// Unix epoch milliseconds when the call started.
    pub ts: u64,
    pub tool: String,
    /// `None` for meta-tools; otherwise the owning toolset name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toolset: Option<String>,
    pub dur_ms: u64,
    pub status: CallStatus,
    /// Short error identifier (e.g., "missing_argument", "ipc_unavailable").
    /// Free-form for now; promoted to a real enum in a follow-up change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    /// Serialized size of the arguments JSON in bytes.
    pub args_bytes: usize,
    /// Serialized size of the result in bytes (content text + image data).
    pub result_bytes: usize,
}

/// Aggregated counts for a single tool across the session.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolStats {
    pub total: u64,
    pub errors: u64,
    pub partial: u64,
    pub uncertain: u64,
    pub total_duration_ms: u64,
    /// Last call's status for quick "is it healthy right now?" checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

// ─── Observer ────────────────────────────────────────────────────────────────

/// Centralized call sink. Cheap to clone — all state is behind `Arc`s.
#[derive(Clone)]
pub struct CallObserver {
    inner: Arc<Inner>,
}

struct Inner {
    recent: Mutex<VecDeque<CallRecord>>,
    per_tool: Mutex<HashMap<String, ToolStats>>,
    total_calls: AtomicU64,
    error_calls: AtomicU64,
    partial_calls: AtomicU64,
    uncertain_calls: AtomicU64,
    started_at_ms: u64,
    started_instant: Instant,
    log_path: Option<PathBuf>,
}

impl CallObserver {
    /// Create an observer with in-memory counters and optional bounded JSONL.
    pub fn new(log_path: Option<PathBuf>) -> Self {
        let now_ms = unix_ms();

        CallObserver {
            inner: Arc::new(Inner {
                recent: Mutex::new(VecDeque::with_capacity(MAX_RECENT_CALLS)),
                per_tool: Mutex::new(HashMap::new()),
                total_calls: AtomicU64::new(0),
                error_calls: AtomicU64::new(0),
                partial_calls: AtomicU64::new(0),
                uncertain_calls: AtomicU64::new(0),
                started_at_ms: now_ms,
                started_instant: Instant::now(),
                log_path,
            }),
        }
    }

    /// Record a completed call. Never fails — logs warnings on IO issues.
    pub async fn record(&self, rec: CallRecord) {
        // Update counters.
        self.inner.total_calls.fetch_add(1, Ordering::Relaxed);
        if matches!(rec.status, CallStatus::Error | CallStatus::Uncertain) {
            self.inner.error_calls.fetch_add(1, Ordering::Relaxed);
        }
        if matches!(rec.status, CallStatus::Partial) {
            self.inner.partial_calls.fetch_add(1, Ordering::Relaxed);
        }
        if matches!(rec.status, CallStatus::Uncertain) {
            self.inner.uncertain_calls.fetch_add(1, Ordering::Relaxed);
        }

        // Update per-tool stats.
        {
            let mut per_tool = self.inner.per_tool.lock().await;
            let entry = per_tool.entry(rec.tool.clone()).or_default();
            entry.total += 1;
            entry.total_duration_ms += rec.dur_ms;
            entry.last_status = Some(rec.status.as_str().to_string());
            if matches!(rec.status, CallStatus::Error | CallStatus::Uncertain) {
                entry.errors += 1;
                entry.last_error = rec.error_kind.clone();
            }
            if matches!(rec.status, CallStatus::Partial) {
                entry.partial += 1;
            }
            if matches!(rec.status, CallStatus::Uncertain) {
                entry.uncertain += 1;
            }
        }

        // Push to ring buffer.
        {
            let mut ring = self.inner.recent.lock().await;
            if ring.len() >= MAX_RECENT_CALLS {
                ring.pop_front();
            }
            ring.push_back(rec.clone());
        }

        if let Some(path) = self.inner.log_path.clone() {
            let written = crate::tools::blocking_work::spawn_tracked_blocking(move || {
                append_jsonl(&path, &rec, MAX_CALL_LOG_BYTES)
            })
            .await;
            match written {
                Ok(Ok(())) => {}
                Ok(Err(error)) => warn!(%error, "JSONL write failed"),
                Err(error) => warn!(%error, "JSONL writer stopped"),
            }
        }
    }

    /// Return up to `limit` most-recent calls (newest first). `limit == 0` means
    /// the ring's current capacity (up to `MAX_RECENT_CALLS`).
    pub async fn recent(&self, limit: usize) -> Vec<CallRecord> {
        let ring = self.inner.recent.lock().await;
        let n = if limit == 0 {
            ring.len()
        } else {
            limit.min(ring.len())
        };
        ring.iter().rev().take(n).cloned().collect()
    }

    /// Snapshot of current stats for the `server_stats` meta-tool.
    pub async fn snapshot(&self) -> StatsSnapshot {
        let per_tool = self.inner.per_tool.lock().await.clone();
        let uptime_ms = self.inner.started_instant.elapsed().as_millis() as u64;
        StatsSnapshot {
            started_at_ms: self.inner.started_at_ms,
            uptime_ms,
            total_calls: self.inner.total_calls.load(Ordering::Relaxed),
            error_calls: self.inner.error_calls.load(Ordering::Relaxed),
            partial_calls: self.inner.partial_calls.load(Ordering::Relaxed),
            uncertain_calls: self.inner.uncertain_calls.load(Ordering::Relaxed),
            log_path: self
                .inner
                .log_path
                .as_ref()
                .map(|p| p.display().to_string()),
            per_tool,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsSnapshot {
    pub started_at_ms: u64,
    pub uptime_ms: u64,
    pub total_calls: u64,
    pub error_calls: u64,
    pub partial_calls: u64,
    pub uncertain_calls: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    pub per_tool: HashMap<String, ToolStats>,
}

// ─── Path helpers ────────────────────────────────────────────────────────────

/// Default JSONL path for standalone and embedded servers with logging enabled.
pub fn default_calls_log_path() -> Option<PathBuf> {
    let state = match konnect_sexp::state::StateDir::resolve() {
        Ok(state) => state,
        Err(error) => {
            warn!(error, "call log unavailable");
            return None;
        }
    };
    Some(state.root().join("logs/calls.jsonl"))
}

fn append_jsonl(path: &Path, record: &CallRecord, limit: u64) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    let mut line =
        serde_json::to_vec(record).map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    line.push(b'\n');
    let size = line.len() as u64;
    if size > limit {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "call log record exceeds the size limit",
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Rotate by name while holding a stable lock shared by all Konnect processes.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("jsonl.lock"))?;
    lock.lock()?;
    let archived = path.with_extension("jsonl.1");
    if log_size(&archived)? > limit {
        retire_oversized(&archived)?;
    }
    let current_size = log_size(path)?;
    if current_size > limit {
        retire_oversized(path)?;
    } else if current_size > limit - size {
        match std::fs::remove_file(&archived) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::rename(path, &archived)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let start = file.metadata()?.len();
    if let Err(error) = file.write_all(&line).and_then(|()| file.flush()) {
        // Windows append handles cannot truncate; reopen while holding the sidecar lock.
        drop(file);
        OpenOptions::new().write(true).open(path)?.set_len(start)?;
        return Err(error);
    }
    Ok(())
}

fn retire_oversized(path: &Path) -> std::io::Result<()> {
    let dir = tempfile::Builder::new()
        .prefix("calls-legacy-")
        .tempdir_in(path.parent().unwrap_or(Path::new(".")))?;
    std::fs::rename(path, dir.path().join("calls.jsonl"))?;
    let kept = dir.keep();
    warn!(path = %kept.display(), "preserved unbounded call log outside rotation");
    Ok(())
}

fn log_size(path: &Path) -> std::io::Result<u64> {
    match std::fs::metadata(path) {
        Ok(info) => Ok(info.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

// ─── Utilities ───────────────────────────────────────────────────────────────

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Generate a short call id. We don't pull in `uuid` here — 16 hex chars of
/// randomness from the running thread's nanosecond + atomic counter is plenty
/// for disambiguating a few hundred calls per session.
pub fn new_call_id() -> String {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let seq = CTR.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    format!("{:08x}{:08x}", nanos, seq)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(tool: &str, status: CallStatus) -> CallRecord {
        CallRecord {
            call_id: new_call_id(),
            ts: unix_ms(),
            tool: tool.to_string(),
            toolset: Some("test".to_string()),
            dur_ms: 5,
            status,
            error_kind: if matches!(status, CallStatus::Error | CallStatus::Uncertain) {
                Some("boom".to_string())
            } else {
                None
            },
            args_bytes: 10,
            result_bytes: 20,
        }
    }

    #[tokio::test]
    async fn ring_buffer_bounds_at_max() {
        let obs = CallObserver::new(None);
        for _ in 0..(MAX_RECENT_CALLS + 50) {
            obs.record(sample_record("t", CallStatus::Ok)).await;
        }
        let recent = obs.recent(0).await;
        assert_eq!(recent.len(), MAX_RECENT_CALLS);
    }

    #[tokio::test]
    async fn per_tool_stats_track_errors() {
        let obs = CallObserver::new(None);
        obs.record(sample_record("add_wire", CallStatus::Ok)).await;
        obs.record(sample_record("add_wire", CallStatus::Ok)).await;
        obs.record(sample_record("add_wire", CallStatus::Error))
            .await;
        obs.record(sample_record("route_trace", CallStatus::Error))
            .await;

        let snap = obs.snapshot().await;
        assert_eq!(snap.total_calls, 4);
        assert_eq!(snap.error_calls, 2);

        let aw = snap.per_tool.get("add_wire").unwrap();
        assert_eq!(aw.total, 3);
        assert_eq!(aw.errors, 1);
        assert_eq!(aw.last_status.as_deref(), Some("error"));
        assert_eq!(aw.last_error.as_deref(), Some("boom"));

        let rt = snap.per_tool.get("route_trace").unwrap();
        assert_eq!(rt.errors, 1);
    }

    #[tokio::test]
    async fn partial_and_uncertain_calls_are_accounted_separately() {
        let obs = CallObserver::new(None);
        obs.record(sample_record("batch_place_components", CallStatus::Partial))
            .await;
        obs.record(sample_record(
            "batch_place_components",
            CallStatus::Uncertain,
        ))
        .await;

        let snap = obs.snapshot().await;
        assert_eq!(snap.total_calls, 2);
        assert_eq!(snap.partial_calls, 1);
        assert_eq!(snap.uncertain_calls, 1);
        assert_eq!(snap.error_calls, 1, "uncertain is still error-like");
        let stats = snap.per_tool.get("batch_place_components").unwrap();
        assert_eq!(stats.partial, 1);
        assert_eq!(stats.uncertain, 1);
        assert_eq!(stats.last_status.as_deref(), Some("uncertain"));
    }

    #[tokio::test]
    async fn jsonl_append_roundtrip() {
        // Use a tempdir so the observer can freely create + append to a file
        // inside it without fighting Windows delete-on-close semantics.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("calls.jsonl");
        let obs = CallObserver::new(Some(path.clone()));

        obs.record(sample_record("add_wire", CallStatus::Ok)).await;
        obs.record(sample_record("add_wire", CallStatus::Error))
            .await;

        let contents = std::fs::read_to_string(&path).expect("log read");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"status\":\"ok\""));
        assert!(lines[1].contains("\"status\":\"error\""));
        assert!(lines[1].contains("\"error_kind\":\"boom\""));
    }

    #[test]
    fn call_log_rotation_caps_both_files_and_preserves_oversized_legacy_logs() {
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("calls.jsonl");
        let archived = active.with_extension("jsonl.1");
        let record = sample_record("rotate", CallStatus::Ok);
        let limit = serde_json::to_vec(&record).unwrap().len() as u64 + 1;
        for path in [&active, &archived] {
            std::fs::write(path, vec![b'x'; (limit + 1) as usize]).unwrap();
        }
        append_jsonl(&active, &record, limit).unwrap();
        assert!(!archived.exists());
        let legacy = dir
            .path()
            .read_dir()
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("calls-legacy-")
            })
            .collect::<Vec<_>>();
        assert_eq!(legacy.len(), 2);
        for entry in legacy {
            assert_eq!(
                log_size(&entry.path().join("calls.jsonl")).unwrap(),
                limit + 1
            );
        }
        append_jsonl(&active, &record, limit).unwrap();
        for path in [&active, &archived] {
            assert_eq!(log_size(path).unwrap(), limit);
            let data = std::fs::read_to_string(path).unwrap();
            assert_eq!(data.lines().count(), 1);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&data).unwrap()["tool"],
                "rotate"
            );
        }
        assert!(append_jsonl(&active, &record, limit - 1).is_err());
        assert_eq!(log_size(&active).unwrap(), limit);
    }

    #[test]
    fn concurrent_processes_share_the_call_log_rotation_lock() {
        const CHILD_PATH: &str = "KONNECT_TEST_CALL_LOG_PROCESS_PATH";
        let record = sample_record("shared", CallStatus::Ok);
        let limit = 4 * (serde_json::to_vec(&record).unwrap().len() as u64 + 1);
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            for _ in 0..12 {
                append_jsonl(Path::new(&path), &record, limit).unwrap();
            }
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("calls.jsonl");
        let mut children = (0..2).map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "observability::tests::concurrent_processes_share_the_call_log_rotation_lock"])
                .env(CHILD_PATH, &active)
                .stdout(std::process::Stdio::null())
                .spawn().unwrap()
        }).collect::<Vec<_>>();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        for path in [active.clone(), active.with_extension("jsonl.1")] {
            assert_eq!(log_size(&path).unwrap(), limit);
            let data = std::fs::read_to_string(path).unwrap();
            assert_eq!(data.lines().count(), 4);
            for line in data.lines() {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(line).unwrap()["tool"],
                    "shared"
                );
            }
        }
    }

    #[test]
    fn call_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(new_call_id()));
        }
    }
}
