//! `config` toolset — User preferences, project rules, and effective configuration.
//!
//! Persists user-level config to `~/.konnect/config.json` and project-level
//! config to `<project_dir>/.konnect/project.json`. Claude should call
//! `load_user_config` at the start of every session.
//!
//! A configuration file has four states and they are never merged (#580):
//! absent (the defaults, reported as such), loaded, malformed, and unreadable.
//! Only an absent file may read as the defaults. A file that exists and cannot
//! be used is a refusal, because the next save would otherwise persist those
//! defaults over the user's own settings.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{invalid_arg, require_str, ToolContext, ToolDef};
use konnect_sexp::writer::{write_atomic_if_unchanged, write_new_atomic};
use konnect_sexp::SexpError;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tracing::info;

// ─── Default config ──────────────────────────────────────────────────────────

fn default_user_config() -> serde_json::Value {
    json!({
        "preferred_manufacturers": [],
        "preferred_distributors": ["JLCPCB", "LCSC"],
        "default_passives": {
            "decoupling_cap": "100nF X7R 0402",
            "pull_up": "10k 0402",
            "bulk_cap": "10uF X5R 0805"
        },
        "fab_constraints": {
            "min_trace_width_mm": 0.15,
            "min_via_drill_mm": 0.3,
            "min_clearance_mm": 0.15,
            "layer_count": 2,
            "fab_house": "JLCPCB"
        },
        "naming_conventions": {
            "net_prefix_power": "VCC_",
            "net_prefix_ground": "GND"
        },
        "design_rules": [],
        // Sourcing policy consumed by the upcoming derating/AVL checks.
        // Derating limits are max operating/rated utilization — conservative
        // general practice, not MIL-HDBK-217. An empty AVL means "not
        // enforced", which those checks report as a warning, never a pass.
        "sourcing": {
            "avl": [],
            "derating": {
                "capacitor": { "voltage": 0.80 },
                "resistor": { "power": 0.60 },
                "inductor": { "current": 0.80 },
                "mosfet": { "vds": 0.80, "id": 0.80 },
                "diode": { "vr": 0.80, "if": 0.80 },
                "led": { "if": 0.80 },
                "connector": { "current": 0.80 },
                "regulator": { "power": 0.70, "current": 0.80 }
            }
        }
    })
}

fn default_project_config() -> serde_json::Value {
    json!({
        "design_rules": [],
        "fab_constraints": {},
        "naming_conventions": {},
        // Project-side sourcing overrides. NOTE: deep_merge REPLACES arrays,
        // so a project-level "avl" supersedes the user list entirely.
        // Design rules are combined by the effective view separately.
        "sourcing": {}
    })
}

// ─── Config file paths ───────────────────────────────────────────────────────

fn user_config_path() -> anyhow::Result<PathBuf> {
    use konnect_sexp::state::{legacy_roaming_dir, StateDir};

    let state = StateDir::resolve().map_err(anyhow::Error::msg)?;
    Ok(state.existing_or_default("config.json", || {
        legacy_roaming_dir().map(|dir| dir.join("config.json"))
    }))
}

fn project_config_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".konnect").join("project.json")
}

// ─── Config I/O helpers ──────────────────────────────────────────────────────

/// What a configuration file on disk turned out to be.
#[derive(Debug)]
enum ConfigRead {
    /// No file. The one state that may read as the defaults.
    Absent,
    /// A JSON object, with the exact text a conditional save must still find.
    Loaded {
        value: serde_json::Value,
        text: String,
    },
    /// The file exists and is not a JSON object.
    Malformed { reason: String },
    /// The file exists, or may, and could not be read: permissions, a
    /// directory at the path, invalid UTF-8.
    Unreadable { reason: String },
}

fn read_config(path: &Path) -> ConfigRead {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return ConfigRead::Absent,
        Err(error) => {
            return ConfigRead::Unreadable {
                reason: error.to_string(),
            }
        }
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) if value.is_object() => ConfigRead::Loaded { value, text },
        Ok(other) => ConfigRead::Malformed {
            reason: format!(
                "the document root is {}, not an object",
                json_type_name(&other)
            ),
        },
        Err(error) => ConfigRead::Malformed {
            reason: error.to_string(),
        },
    }
}

/// Where a reported configuration came from.
const FROM_FILE: &str = "file";
const FROM_DEFAULTS: &str = "defaults";
/// No project directory was given or configured, so there is no project file
/// to read and the project half is the defaults.
const FROM_NO_PROJECT: &str = "not_configured";

/// A configuration to work from: the document, its origin, and the exact text
/// on disk when there is one.
struct UsableConfig {
    value: serde_json::Value,
    source: &'static str,
    text: Option<String>,
}

fn usable_config(
    path: &Path,
    which: &str,
    default: serde_json::Value,
) -> Result<UsableConfig, CallToolResult> {
    match read_config(path) {
        ConfigRead::Absent => Ok(UsableConfig {
            value: default,
            source: FROM_DEFAULTS,
            text: None,
        }),
        ConfigRead::Loaded { value, text } => Ok(UsableConfig {
            value,
            source: FROM_FILE,
            text: Some(text),
        }),
        ConfigRead::Malformed { reason } => Err(unusable_config(
            path,
            which,
            format!("malformed_json: {reason}"),
        )),
        ConfigRead::Unreadable { reason } => Err(unusable_config(
            path,
            which,
            format!("unreadable: {reason}"),
        )),
    }
}

/// The refusal for a configuration file that exists and cannot be used.
fn unusable_config(path: &Path, which: &str, reason: String) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::InvalidConfiguration {
            path: path.display().to_string(),
            reason: reason.clone(),
        },
        format!(
            "The {which} configuration at '{}' exists but cannot be used ({reason}). Konnect did \
             not substitute defaults and wrote nothing, because saving over it would replace \
             your settings. Repair or move the file, then retry.",
            path.display()
        ),
    )
}

/// A persistence step whose result cannot say what is on disk.
fn config_outcome_uncertain(path: &Path, operation: &str, reason: String) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::MutationOutcomeUncertain {
            operation: operation.to_string(),
            path: path.display().to_string(),
            reason: reason.clone(),
        },
        format!(
            "{operation} could not prove what is now on disk at '{}': {reason}. The file may or \
             may not have been replaced; read it before retrying.",
            path.display()
        ),
    )
}

/// Create the file when none was read, replace it only if it still holds the
/// text that was read.
fn persist_config(path: &Path, expected: Option<&str>, content: &str) -> Result<(), SexpError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_config(path, expected, content)
}

/// Create only the settings subdirectory. If the project vanished after
/// validation, do not recreate it as a side effect of saving preferences.
fn persist_project_config(
    path: &Path,
    expected: Option<&str>,
    content: &str,
) -> Result<(), SexpError> {
    if let Some(parent) = path.parent() {
        match std::fs::create_dir(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    write_config(path, expected, content)
}

fn write_config(path: &Path, expected: Option<&str>, content: &str) -> Result<(), SexpError> {
    match expected {
        Some(expected) => write_atomic_if_unchanged(path, expected, content),
        None => write_new_atomic(path, content),
    }
}

/// What [`update_config`] committed, read back from disk.
struct SavedConfig {
    value: serde_json::Value,
    created: bool,
}

/// Apply `change` to one configuration file and persist it.
///
/// Persistence is injectable so a test can fail it after a real replacement
/// without racing an editor; production passes [`persist_config`].
fn update_config(
    path: &Path,
    which: &str,
    default: serde_json::Value,
    operation: &str,
    change: impl FnOnce(&mut serde_json::Value) -> Result<(), CallToolResult>,
    persist: impl FnOnce(&Path, Option<&str>, &str) -> Result<(), SexpError>,
) -> Result<SavedConfig, CallToolResult> {
    let UsableConfig {
        mut value, text, ..
    } = usable_config(path, which, default)?;
    change(&mut value)?;
    let content = serde_json::to_string_pretty(&value)
        .map_err(|error| CallToolResult::error(format!("{operation}: {error}")))?;
    let created = text.is_none();

    match persist(path, text.as_deref(), &content) {
        Ok(()) => {}
        // An atomic no-clobber create that finds a file never replaced
        // anything: that is positive evidence nothing was written.
        Err(SexpError::Io(error))
            if created && error.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            return Err(CallToolResult::error_kind(
                ToolErrorKind::Conflict {
                    paths: vec![path.display().to_string()],
                },
                format!(
                    "A {which} configuration appeared at '{}' while it was being created; \
                     nothing was written. Retry to update the file that is there now.",
                    path.display()
                ),
            ));
        }
        // Everything else proves nothing about the disk: the conditional
        // writer returns `Conflict` both before a replacement and after one
        // whose readback differs (#579), and an I/O error can follow either.
        Err(error) => return Err(config_outcome_uncertain(path, operation, error.to_string())),
    }

    match read_config(path) {
        ConfigRead::Loaded {
            value: observed, ..
        } if observed == value => Ok(SavedConfig {
            value: observed,
            created,
        }),
        other => Err(config_outcome_uncertain(
            path,
            operation,
            format!("the saved file did not read back as written ({other:?})"),
        )),
    }
}

/// Resolve a requested setting against the keys and shapes Konnect defines.
fn config_key_schema<'a>(defaults: &'a Value, key_path: &str) -> anyhow::Result<&'a Value> {
    let mut entry = defaults;
    for key in key_path.split('.') {
        anyhow::ensure!(
            !key.trim().is_empty(),
            "Configuration path has an empty segment"
        );
        let object = entry
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("'{key_path}' traverses a non-object setting"))?;
        entry = object.get(key).ok_or_else(|| {
            let candidates = object
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::anyhow!("Unknown key '{key}' in '{key_path}'; expected one of {candidates}")
        })?;
    }
    Ok(entry)
}

fn validate_config_value(key_path: &str, schema: &Value, value: &Value) -> anyhow::Result<()> {
    anyhow::ensure!(
        json_type_name(schema) == json_type_name(value),
        "'{key_path}' must be {}",
        json_type_name(schema)
    );
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            anyhow::ensure!(
                !key.trim().is_empty() && !key.contains('.'),
                "'{key_path}' contains a key that is not a single path segment"
            );
            let entry = config_key_schema(schema, key)?;
            validate_config_value(&format!("{key_path}.{key}"), entry, value)?;
        }
    } else if let Some(items) = value.as_array() {
        let mut rules = std::collections::BTreeSet::new();
        for item in items {
            let text = item
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("'{key_path}' must contain only strings"))?;
            if key_path == "design_rules" {
                let text = text.trim();
                anyhow::ensure!(!text.is_empty(), "Design rules must not be blank");
                anyhow::ensure!(
                    rules.insert(text),
                    "Design rules must be unique within a scope"
                );
            }
        }
    } else if key_path == "fab_constraints.layer_count" {
        anyhow::ensure!(
            value.as_u64().is_some_and(|layers| layers > 0),
            "'{key_path}' must be a positive integer"
        );
    } else if value.is_number() {
        let limit = if key_path.starts_with("sourcing.derating.") {
            1.0
        } else {
            f64::MAX
        };
        anyhow::ensure!(
            value
                .as_f64()
                .is_some_and(|number| (0.0..=limit).contains(&number)),
            "'{key_path}' must be a nonnegative number within its supported range"
        );
    }
    Ok(())
}

/// Validate the requested change while leaving unrelated existing settings alone.
fn set_key(
    key_path: &str,
    value: Value,
) -> impl FnOnce(&mut Value) -> Result<(), CallToolResult> + '_ {
    move |config| {
        let defaults = default_user_config();
        let schema = config_key_schema(&defaults, key_path)
            .map_err(|error| invalid_arg("key_path", &error.to_string()))?;
        if !value.is_null() {
            validate_config_value(key_path, schema, &value)
                .map_err(|error| invalid_arg("value", &error.to_string()))?;
        }
        set_dot_path(config, key_path, value)
            .map_err(|error| invalid_arg("key_path", &error.to_string()))
    }
}

/// The `change` that appends one design rule.
fn push_rule(rule: &str) -> impl FnOnce(&mut Value) -> Result<(), CallToolResult> + '_ {
    move |config| {
        if config.get("design_rules").is_none() {
            config["design_rules"] = json!([]);
        }
        let rules = config["design_rules"].as_array_mut().ok_or_else(|| {
            invalid_arg("rule", "the existing design_rules setting is not an array")
        })?;
        if rules
            .iter()
            .any(|stored| stored.as_str().is_some_and(|stored| stored.trim() == rule))
        {
            return Err(invalid_arg(
                "rule",
                "this design rule already exists in this scope",
            ));
        }
        rules.push(json!(rule));
        Ok(())
    }
}

fn design_rules(config: &serde_json::Value) -> Vec<String> {
    config["design_rules"]
        .as_array()
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| rule.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Deep merge: overlay values onto base. overlay takes precedence.
fn deep_merge(base: &serde_json::Value, overlay: &serde_json::Value) -> serde_json::Value {
    match (base, overlay) {
        (serde_json::Value::Object(b), serde_json::Value::Object(o)) => {
            let mut merged = b.clone();
            for (key, val) in o {
                let base_val = merged.get(key).cloned().unwrap_or(serde_json::Value::Null);
                merged.insert(key.clone(), deep_merge(&base_val, val));
            }
            serde_json::Value::Object(merged)
        }
        (_, overlay) if !overlay.is_null() => overlay.clone(),
        (base, _) => base.clone(),
    }
}

/// Set or delete a dot-notation key, creating missing objects only on set.
fn set_dot_path(config: &mut Value, key_path: &str, value: Value) -> anyhow::Result<()> {
    let parts: Vec<_> = key_path.split('.').collect();
    let (last, parents) = parts
        .split_last()
        .expect("splitting a string yields at least one part");
    let mut entry = config;
    for key in parents {
        let map = entry
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("'{key_path}' traverses a non-object setting"))?;
        entry = if value.is_null() {
            map.get_mut(*key)
                .ok_or_else(|| anyhow::anyhow!("'{key_path}' is not set"))?
        } else {
            map.entry(*key).or_insert_with(|| json!({}))
        };
    }
    let map = entry
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("'{key_path}' traverses a non-object setting"))?;
    if value.is_null() {
        anyhow::ensure!(map.remove(*last).is_some(), "'{key_path}' is not set");
    } else {
        map.insert((*last).to_string(), value);
    }
    Ok(())
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ─── Tool definitions ─────────────────────────────────────────────────────────

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "load_user_config",
            "Load the user's global Konnect preferences. Call this at the start of every session \
             to get preferred manufacturers, fab constraints, default passives, and design rules. \
             'source' says whether the answer is the user's file or Konnect's defaults (no file \
             existed; the defaults are then written for the user to edit and 'persisted' says \
             whether that worked). A file that exists but cannot be parsed or read is refused \
             with invalid_configuration and is never replaced by defaults.",
            json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            |args, ctx| async move { handle_load_user_config(args, ctx).await }
        ),
        tool!(
            "save_user_config",
            "Update a user preference. Use dot-notation for nested keys, e.g. 'fab_constraints.fab_house'. \
             Call this when the user says things like 'always use JLCPCB' or 'I prefer 0402 passives'. \
             Every other key is kept, and the response is the file as read back. Refuses with \
             invalid_configuration, writing nothing, when the existing file cannot be parsed or \
             read; a persistence failure is mutation_outcome_uncertain, so read the file before retrying.",
            json!({
                "type": "object",
                "properties": {
                    "key_path": {
                        "type": "string",
                        "description": "Dot-notation path to the config key, e.g. 'fab_constraints.fab_house' or 'default_passives.decoupling_cap'"
                    },
                    "value": {
                        "type": ["string", "number", "array", "object", "boolean", "null"],
                        "additionalProperties": true,
                        "description": "Value for the specified key, or null to delete an existing key"
                    }
                },
                "required": ["key_path", "value"]
            }),
            |args, ctx| async move { handle_save_user_config(args, ctx).await }
        ),
        tool!(
            "load_project_config",
            "Load project-specific configuration from <project_dir>/.konnect/project.json. \
             Project config overrides user config where both exist. 'source' is 'file' or \
             'defaults' (no file; nothing is written). A file that cannot be parsed or read is \
             refused with invalid_configuration.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Path to the KiCAD project directory. If omitted, uses the configured project_dir."
                    }
                },
                "required": []
            }),
            |args, ctx| async move { handle_load_project_config(args, ctx).await }
        ),
        tool!(
            "save_project_config",
            "Save a project-specific rule or override. Same dot-notation as save_user_config \
             but writes to the project's .konnect/project.json, with the same refusals: an \
             unusable existing file is never overwritten.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory (optional, uses configured default)" },
                    "key_path": { "type": "string", "description": "Dot-notation config key" },
                    "value": {
                        "type": ["string", "number", "array", "object", "boolean", "null"],
                        "additionalProperties": true,
                        "description": "Value for the specified key, or null to delete an existing key"
                    }
                },
                "required": ["key_path", "value"]
            }),
            |args, ctx| async move { handle_save_project_config(args, ctx).await }
        ),
        tool!(
            "get_effective_config",
            "Return the merged configuration (user defaults + project overrides). \
             Design rules are ordered user first, then project, with scope in 'design_rule_sources'. \
             This is the config Claude should use for all design decisions. 'sources' says \
             where each half came from ('file', 'defaults', or 'not_configured' when there is no \
             project directory). Refuses with invalid_configuration, naming which file, rather \
             than merging defaults in place of a file that cannot be used.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory (optional)" }
                },
                "required": []
            }),
            |args, ctx| async move { handle_get_effective_config(args, ctx).await }
        ),
        tool!(
            "add_design_rule",
            "Add a natural-language design rule that Claude should follow in this project. \
             Examples: 'Always use 100nF X7R for MCU decoupling within 3mm of power pin', \
             'Route USB D+/D- as 90-ohm differential pair'.",
            json!({
                "type": "object",
                "properties": {
                    "rule": { "type": "string", "description": "The design rule in plain English" },
                    "scope": {
                        "type": "string",
                        "enum": ["user", "project"],
                        "description": "'user' (applies to all projects) or 'project' (this project only)",
                        "default": "project"
                    },
                    "project_dir": { "type": "string", "description": "Project directory (for project-scoped rules)" }
                },
                "required": ["rule"]
            }),
            |args, ctx| async move { handle_add_design_rule(args, ctx).await }
        ),
        tool!(
            "list_design_rules",
            "List all active design rules (user-level + project-level).",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory (optional)" }
                },
                "required": []
            }),
            |args, ctx| async move { handle_list_design_rules(args, ctx).await }
        ),
    ]
}

// ─── Handlers ─────────────────────────────────────────────────────────────────
//
// Each handler resolves its paths and hands them to a synchronous core, so the
// cores can be driven against a temporary directory without touching the
// developer's own preferences.

fn load_user_config_at(path: &Path) -> CallToolResult {
    let config = match usable_config(path, "user", default_user_config()) {
        Ok(config) => config,
        Err(refusal) => return refusal,
    };
    let mut body = json!({
        "config": config.value,
        "path": path.to_str().unwrap_or(""),
        "source": config.source,
    });
    if config.source == FROM_DEFAULTS {
        // First run: leave the user a file to edit. A failure here is
        // reported, not swallowed; the defaults are still the answer.
        let content = serde_json::to_string_pretty(&body["config"]).unwrap_or_default();
        match persist_config(path, None, &content) {
            Ok(()) => body["persisted"] = json!(true),
            Err(error) => {
                body["persisted"] = json!(false);
                body["persist_error"] = json!(error.to_string());
            }
        }
        body["note"] = json!(
            "No user preferences file existed, so these are Konnect's defaults. Project config \
             may override these values."
        );
    } else {
        body["note"] = json!(
            "User preferences loaded from the file. Project config may override these values."
        );
    }
    CallToolResult::json(&body)
}

async fn handle_load_user_config(
    _args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = user_config_path()?;
    info!(path = %path.display(), "[BETA] Loading user config");
    Ok(load_user_config_at(&path))
}

fn required_value(args: &Value) -> Result<Value, CallToolResult> {
    args.get("value")
        .cloned()
        .ok_or_else(|| invalid_arg("value", "required"))
}

async fn handle_save_user_config(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let key_path = match require_str(args, "key_path") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let value = match required_value(args) {
        Ok(value) => value,
        Err(refusal) => return Ok(refusal),
    };

    let path = user_config_path()?;
    info!(key_path = %key_path, "[BETA] Saving user config");
    Ok(
        match update_config(
            &path,
            "user",
            if value.is_null() {
                json!({})
            } else {
                default_user_config()
            },
            "save_user_config",
            set_key(&key_path, value.clone()),
            persist_config,
        ) {
            Ok(saved) => CallToolResult::json(&json!({
                "updated": key_path,
                "value": value,
                "config": saved.value,
                "path": path.to_str().unwrap_or(""),
                "created": saved.created,
            })),
            Err(refusal) => refusal,
        },
    )
}

async fn handle_load_project_config(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let project_dir = resolve_project_dir(args, ctx)?;
    let path = project_config_path(&project_dir);
    info!(path = %path.display(), "[BETA] Loading project config");
    Ok(
        match usable_config(&path, "project", default_project_config()) {
            Ok(config) => CallToolResult::json(&json!({
                "config": config.value,
                "project_dir": project_dir.to_str().unwrap_or(""),
                "path": path.to_str().unwrap_or(""),
                "source": config.source,
            })),
            Err(refusal) => refusal,
        },
    )
}

async fn handle_save_project_config(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let project_dir = match write_project_dir(args, ctx) {
        Ok(dir) => dir,
        Err(refusal) => return Ok(refusal),
    };
    let key_path = match require_str(args, "key_path") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let value = match required_value(args) {
        Ok(value) => value,
        Err(refusal) => return Ok(refusal),
    };

    let path = project_config_path(&project_dir);
    Ok(
        match update_config(
            &path,
            "project",
            if value.is_null() {
                json!({})
            } else {
                default_project_config()
            },
            "save_project_config",
            set_key(&key_path, value.clone()),
            persist_project_config,
        ) {
            Ok(saved) => CallToolResult::json(&json!({
                "updated": key_path,
                "value": value,
                "project_dir": project_dir.to_str().unwrap_or(""),
                "path": path.to_str().unwrap_or(""),
                "created": saved.created,
            })),
            Err(refusal) => refusal,
        },
    )
}

/// The user and project documents both tools below read, or the refusal for
/// whichever of them cannot be used.
fn both_configs(
    user_path: &Path,
    project_path: Option<&Path>,
) -> Result<(UsableConfig, UsableConfig), CallToolResult> {
    let user = usable_config(user_path, "user", default_user_config())?;
    let project = match project_path {
        Some(path) => usable_config(path, "project", default_project_config())?,
        None => UsableConfig {
            value: default_project_config(),
            source: FROM_NO_PROJECT,
            text: None,
        },
    };
    Ok((user, project))
}

fn effective_config_at(user_path: &Path, project_path: Option<&Path>) -> CallToolResult {
    let (user, project) = match both_configs(user_path, project_path) {
        Ok(configs) => configs,
        Err(refusal) => return refusal,
    };
    let scoped_rules: Vec<_> = design_rules(&user.value)
        .into_iter()
        .map(|rule| (rule, "user"))
        .chain(
            design_rules(&project.value)
                .into_iter()
                .map(|rule| (rule, "project")),
        )
        .collect();
    let mut effective = if project.source == FROM_FILE {
        deep_merge(&user.value, &project.value)
    } else {
        user.value
    };
    effective["design_rules"] = json!(scoped_rules
        .iter()
        .map(|(rule, _)| rule)
        .collect::<Vec<_>>());
    let design_rule_sources: Vec<_> = scoped_rules
        .iter()
        .map(|(rule, source)| json!({ "rule": rule, "source": source }))
        .collect();
    CallToolResult::json(&json!({
        "effective_config": effective,
        "design_rule_sources": design_rule_sources,
        "sources": { "user": user.source, "project": project.source },
        "note": "Merged user defaults + project overrides. Use these values for all design decisions."
    }))
}

async fn handle_get_effective_config(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let project_path = resolve_project_dir(args, ctx)
        .ok()
        .map(|dir| project_config_path(&dir));
    Ok(effective_config_at(
        &user_config_path()?,
        project_path.as_deref(),
    ))
}

async fn handle_add_design_rule(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let rule = match require_str(args, "rule") {
        Ok(rule) if !rule.trim().is_empty() => rule.trim(),
        Ok(_) => return Ok(invalid_arg("rule", "must not be blank")),
        Err(refusal) => return Ok(refusal),
    };
    let scope = match args.get("scope") {
        None => "project",
        Some(scope) => match scope.as_str() {
            Some(scope @ ("user" | "project")) => scope,
            _ => return Ok(invalid_arg("scope", "must be user or project")),
        },
    };

    let (path, default) = if scope == "user" {
        (user_config_path()?, default_user_config())
    } else {
        let project_dir = match write_project_dir(args, ctx) {
            Ok(dir) => dir,
            Err(refusal) => return Ok(refusal),
        };
        (project_config_path(&project_dir), default_project_config())
    };
    Ok(
        match update_config(
            &path,
            scope,
            default,
            "add_design_rule",
            push_rule(rule),
            |path, expected, content| {
                if scope == "user" {
                    persist_config(path, expected, content)
                } else {
                    persist_project_config(path, expected, content)
                }
            },
        ) {
            Ok(saved) => CallToolResult::json(&json!({
                "added_rule": rule,
                "scope": scope,
                "path": path.to_str().unwrap_or(""),
                "created": saved.created,
            })),
            Err(refusal) => refusal,
        },
    )
}

fn list_design_rules_at(user_path: &Path, project_path: Option<&Path>) -> CallToolResult {
    let (user, project) = match both_configs(user_path, project_path) {
        Ok(configs) => configs,
        Err(refusal) => return refusal,
    };
    let user_rules = design_rules(&user.value);
    let project_rules = design_rules(&project.value);
    CallToolResult::json(&json!({
        "user_rules": user_rules,
        "project_rules": project_rules,
        "total": user_rules.len() + project_rules.len(),
        "sources": { "user": user.source, "project": project.source },
    }))
}

async fn handle_list_design_rules(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let project_path = resolve_project_dir(args, ctx)
        .ok()
        .map(|dir| project_config_path(&dir));
    Ok(list_design_rules_at(
        &user_config_path()?,
        project_path.as_deref(),
    ))
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn write_project_dir(args: &Value, ctx: &ToolContext) -> Result<PathBuf, CallToolResult> {
    if args
        .get("project_dir")
        .is_some_and(|value| value.as_str().is_none_or(|dir| dir.trim().is_empty()))
    {
        return Err(invalid_arg(
            "project_dir",
            "must name an existing directory",
        ));
    }
    let dir = resolve_project_dir(args, ctx)
        .map_err(|error| invalid_arg("project_dir", &error.to_string()))?;
    if !dir.is_dir() {
        return Err(invalid_arg(
            "project_dir",
            "must name an existing directory",
        ));
    }
    Ok(dir)
}

fn resolve_project_dir(args: &serde_json::Value, ctx: &ToolContext) -> anyhow::Result<PathBuf> {
    if let Some(dir) = args["project_dir"].as_str() {
        return Ok(PathBuf::from(dir));
    }
    if let Some(ref dir) = ctx.config.project_dir {
        return Ok(dir.clone());
    }
    anyhow::bail!("No project directory specified. Pass 'project_dir' or configure a default.")
}

#[cfg(test)]
mod dot_path_and_merge_tests {
    use super::*;

    #[test]
    fn deep_merge_overlays_nested_object_keys() {
        let base = json!({
            "fab_constraints": { "fab_house": "JLCPCB", "layer_count": 2 },
            "design_rules": []
        });
        let overlay = json!({
            "fab_constraints": { "layer_count": 4 }
        });

        let merged = deep_merge(&base, &overlay);

        assert_eq!(merged["fab_constraints"]["fab_house"], "JLCPCB");
        assert_eq!(merged["fab_constraints"]["layer_count"], 4);
        assert_eq!(merged["design_rules"], json!([]));
    }

    /// The sourcing policy the derating/AVL checks read must exist in the
    /// defaults with the documented limits — a missing key would make those
    /// checks silently unenforceable, the #218 class.
    #[test]
    fn default_sourcing_policy_is_present_with_conservative_limits() {
        let user = default_user_config();
        assert_eq!(user["sourcing"]["avl"], json!([]));
        assert_eq!(user["sourcing"]["derating"]["capacitor"]["voltage"], 0.80);
        assert_eq!(user["sourcing"]["derating"]["resistor"]["power"], 0.60);
        assert_eq!(user["sourcing"]["derating"]["regulator"]["power"], 0.70);
        assert_eq!(default_project_config()["sourcing"], json!({}));
    }

    /// A project-level AVL REPLACES the user list (deep_merge array
    /// semantics) — pinned so a future "append" change is a decision, not
    /// an accident.
    #[test]
    fn project_avl_replaces_user_avl_wholesale() {
        let user = json!({ "sourcing": { "avl": ["Murata", "TDK"] } });
        let project = json!({ "sourcing": { "avl": ["Vishay"] } });
        let merged = deep_merge(&user, &project);
        assert_eq!(merged["sourcing"]["avl"], json!(["Vishay"]));
    }

    #[test]
    fn deep_merge_null_overlay_value_keeps_base() {
        let base = json!({ "fab_house": "JLCPCB" });
        let overlay = json!({ "fab_house": null });

        let merged = deep_merge(&base, &overlay);

        assert_eq!(merged["fab_house"], "JLCPCB");
    }

    #[test]
    fn set_dot_path_sets_top_level_key() {
        let mut config = json!({});
        set_dot_path(&mut config, "fab_house", json!("JLCPCB")).expect("should succeed");
        assert_eq!(config["fab_house"], "JLCPCB");
    }

    #[test]
    fn set_dot_path_creates_missing_intermediate_objects() {
        let mut config = json!({});
        set_dot_path(&mut config, "fab_constraints.fab_house", json!("JLCPCB"))
            .expect("should succeed");
        assert_eq!(config["fab_constraints"]["fab_house"], "JLCPCB");
    }

    #[test]
    fn set_dot_path_overwrites_existing_nested_value() {
        let mut config = json!({ "fab_constraints": { "fab_house": "PCBWay" } });
        set_dot_path(&mut config, "fab_constraints.fab_house", json!("JLCPCB"))
            .expect("should succeed");
        assert_eq!(config["fab_constraints"]["fab_house"], "JLCPCB");
    }

    #[test]
    fn set_dot_path_errors_instead_of_panicking_on_non_object_root() {
        // Regression test: a corrupted config file that parses as valid JSON
        // but isn't a `{...}` object used to make this function panic via
        // `.unwrap()` on a failed `get_mut`, crashing the whole server.
        let mut config = json!(null);
        let result = set_dot_path(&mut config, "fab_constraints.fab_house", json!("JLCPCB"));
        assert!(result.is_err());
    }

    #[test]
    fn set_dot_path_refuses_to_replace_a_scalar_intermediate_segment() {
        let mut config = json!({ "fab_constraints": "JLCPCB" });
        assert!(set_dot_path(&mut config, "fab_constraints.fab_house", json!("PCBWay")).is_err());
        assert_eq!(config, json!({ "fab_constraints": "JLCPCB" }));
    }
}

#[cfg(test)]
mod config_state_tests {
    use super::*;
    use crate::mcp::error::extract_error_kind;
    use crate::mcp::protocol::ToolContent;

    /// A truncated document that still visibly holds the user's settings.
    const TRUNCATED: &str =
        r#"{"kicad_cli": "D:/tools/kicad-cli.exe", "sourcing": {"avl": ["Vishay", "Murata"]},"#;

    fn body(result: &CallToolResult) -> serde_json::Value {
        match result.content.first() {
            Some(ToolContent::Text { text }) => serde_json::from_str(text).unwrap(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    fn text(result: &CallToolResult) -> String {
        match result.content.first() {
            Some(ToolContent::Text { text }) => text.clone(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    fn config_in(dir: &Path) -> PathBuf {
        dir.join("konnect").join("config.json")
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn the_four_read_states_stay_apart() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_in(dir.path());
        assert!(matches!(read_config(&path), ConfigRead::Absent));

        write(&path, r#"{"a": 1}"#);
        assert!(matches!(read_config(&path), ConfigRead::Loaded { .. }));

        write(&path, TRUNCATED);
        assert!(matches!(read_config(&path), ConfigRead::Malformed { .. }));

        write(&path, "[1, 2]");
        let ConfigRead::Malformed { reason } = read_config(&path) else {
            panic!("a non-object root is not a configuration")
        };
        assert!(reason.contains("array"), "{reason}");

        // A directory where the file should be: present, and not readable.
        let blocked = dir.path().join("blocked").join("config.json");
        std::fs::create_dir_all(&blocked).unwrap();
        assert!(matches!(
            read_config(&blocked),
            ConfigRead::Unreadable { .. }
        ));
    }

    #[test]
    fn an_absent_file_answers_with_the_defaults_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_in(dir.path());
        let result = load_user_config_at(&path);
        assert!(!result.is_error);
        let body = body(&result);
        assert_eq!(body["source"], "defaults", "{body}");
        assert_eq!(body["persisted"], true, "{body}");
        assert_eq!(body["config"], default_user_config());
        assert!(
            matches!(read_config(&path), ConfigRead::Loaded { .. }),
            "the defaults were left on disk for the user to edit"
        );

        let again = body_of_loaded(&path);
        assert_eq!(again["source"], "file", "{again}");
        assert!(again.get("persisted").is_none(), "{again}");
    }

    fn body_of_loaded(path: &Path) -> serde_json::Value {
        body(&load_user_config_at(path))
    }

    #[test]
    fn a_failed_first_persistence_is_reported_not_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        // The parent of the config directory is a file, so it cannot be made.
        let blocker = dir.path().join("konnect");
        std::fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("config.json");

        let result = load_user_config_at(&path);
        // `konnect/config.json` under a *file* reads as unreadable on some
        // platforms and absent on others; both are truthful, neither is "loaded".
        if result.is_error {
            assert_eq!(
                extract_error_kind(&result).as_deref(),
                Some("invalid_configuration")
            );
            return;
        }
        let body = body(&result);
        assert_eq!(body["source"], "defaults", "{body}");
        assert_eq!(body["persisted"], false, "{body}");
        assert!(body["persist_error"].is_string(), "{body}");
    }

    #[test]
    fn a_malformed_file_is_refused_by_load_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_in(dir.path());
        write(&path, TRUNCATED);

        let result = load_user_config_at(&path);
        assert!(result.is_error);
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("invalid_configuration")
        );
        let body = body(&result);
        assert!(
            body["error"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("malformed_json:"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TRUNCATED);
    }

    #[test]
    fn a_save_over_a_malformed_file_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_in(dir.path());
        write(&path, TRUNCATED);

        let refusal = update_config(
            &path,
            "user",
            default_user_config(),
            "save_user_config",
            set_key("fab_constraints.fab_house", json!("PCBWay")),
            persist_config,
        )
        .err()
        .expect("a save after a failed read must refuse");
        assert_eq!(
            extract_error_kind(&refusal).as_deref(),
            Some("invalid_configuration")
        );
        assert!(text(&refusal).contains("wrote nothing"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            TRUNCATED,
            "the user's settings are still there to repair"
        );
    }

    #[test]
    fn a_save_keeps_every_other_key_and_reports_the_readback() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_in(dir.path());
        write(
            &path,
            r#"{"kicad_cli": "D:/tools/kicad-cli.exe", "ui": {"density": "compact"}}"#,
        );

        let saved = update_config(
            &path,
            "user",
            default_user_config(),
            "save_user_config",
            set_key("fab_constraints.fab_house", json!("PCBWay")),
            persist_config,
        )
        .expect("a usable file is updated");
        assert!(!saved.created);
        assert_eq!(saved.value["kicad_cli"], "D:/tools/kicad-cli.exe");
        assert_eq!(saved.value["ui"]["density"], "compact");
        assert_eq!(saved.value["fab_constraints"]["fab_house"], "PCBWay");
        let ConfigRead::Loaded { value, .. } = read_config(&path) else {
            panic!("the saved file parses")
        };
        assert_eq!(value, saved.value, "the response is the committed file");
    }

    #[test]
    fn a_save_with_no_file_creates_one_from_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_in(dir.path());
        let saved = update_config(
            &path,
            "user",
            default_user_config(),
            "save_user_config",
            set_key("fab_constraints.fab_house", json!("PCBWay")),
            persist_config,
        )
        .expect("an absent file is created");
        assert!(saved.created);
        let mut expected = default_user_config();
        expected["fab_constraints"]["fab_house"] = json!("PCBWay");
        assert_eq!(saved.value, expected);
    }

    /// The conditional writer returns `Conflict` both before a replacement and
    /// after one whose readback differs, so neither timing may be reported as
    /// a no-write refusal (#579).
    #[test]
    fn persistence_conflicts_require_inspection_before_retry() {
        for after_replacement in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = config_in(dir.path());
            let original = r#"{"ui": {"density": "compact"}}"#;
            write(&path, original);

            let refusal = update_config(
                &path,
                "user",
                default_user_config(),
                "save_user_config",
                set_key("fab_constraints.fab_house", json!("PCBWay")),
                |target, expected, content| {
                    if after_replacement {
                        persist_config(target, expected, content).unwrap();
                    }
                    Err(SexpError::Conflict {
                        path: target.to_path_buf(),
                    })
                },
            )
            .err()
            .expect("a persistence conflict is not a success");

            assert_eq!(
                extract_error_kind(&refusal).as_deref(),
                Some("mutation_outcome_uncertain")
            );
            let message = text(&refusal);
            assert!(!message.contains("nothing was written"), "{message}");
            assert!(message.contains("read it before retrying"), "{message}");
            let on_disk = std::fs::read_to_string(&path).unwrap();
            assert_eq!(on_disk == original, !after_replacement);
        }
    }

    /// Creating a file that turns out to exist never replaced anything, so
    /// that one case may say nothing was written.
    #[test]
    fn a_file_that_appears_during_creation_is_a_no_write_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_in(dir.path());
        let refusal = update_config(
            &path,
            "user",
            default_user_config(),
            "save_user_config",
            set_key("fab_constraints.fab_house", json!("PCBWay")),
            |_, expected, _| {
                assert!(expected.is_none(), "no file was read, so none is expected");
                Err(SexpError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "appeared",
                )))
            },
        )
        .err()
        .expect("the race is reported");
        assert_eq!(extract_error_kind(&refusal).as_deref(), Some("conflict"));
        assert!(text(&refusal).contains("nothing was written"));
    }

    #[test]
    fn a_readback_that_differs_is_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_in(dir.path());
        write(&path, r#"{"a": 1}"#);
        let refusal = update_config(
            &path,
            "user",
            default_user_config(),
            "save_user_config",
            set_key("fab_constraints.layer_count", json!(2)),
            |target, _, _| {
                std::fs::write(target, r#"{"someone": "else"}"#).unwrap();
                Ok(())
            },
        )
        .err()
        .expect("a file that does not show the change is not a success");
        assert_eq!(
            extract_error_kind(&refusal).as_deref(),
            Some("mutation_outcome_uncertain")
        );
    }

    #[test]
    fn either_unusable_file_refuses_the_merged_view_and_names_which() {
        let dir = tempfile::tempdir().unwrap();
        let user = config_in(dir.path());
        let project = dir
            .path()
            .join("proj")
            .join(".konnect")
            .join("project.json");

        let clean = body(&effective_config_at(&user, Some(&project)));
        assert_eq!(
            clean["sources"],
            json!({ "user": "defaults", "project": "defaults" })
        );
        let no_project = body(&effective_config_at(&user, None));
        assert_eq!(no_project["sources"]["project"], "not_configured");

        write(&project, TRUNCATED);
        let refusal = effective_config_at(&user, Some(&project));
        assert_eq!(
            extract_error_kind(&refusal).as_deref(),
            Some("invalid_configuration")
        );
        assert!(text(&refusal).contains("project configuration"));
        let rules = list_design_rules_at(&user, Some(&project));
        assert_eq!(
            extract_error_kind(&rules).as_deref(),
            Some("invalid_configuration")
        );

        std::fs::remove_file(&project).unwrap();
        write(&user, TRUNCATED);
        let refusal = effective_config_at(&user, Some(&project));
        assert!(text(&refusal).contains("user configuration"));
    }

    #[test]
    fn a_design_rule_is_appended_and_never_over_a_malformed_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".konnect").join("project.json");
        let saved = update_config(
            &path,
            "project",
            default_project_config(),
            "add_design_rule",
            push_rule("Route USB as a 90-ohm pair"),
            persist_config,
        )
        .expect("the update succeeds");
        assert_eq!(
            design_rules(&saved.value),
            vec!["Route USB as a 90-ohm pair".to_string()]
        );

        write(&path, TRUNCATED);
        let refusal = update_config(
            &path,
            "project",
            default_project_config(),
            "add_design_rule",
            push_rule("second"),
            persist_config,
        )
        .err()
        .unwrap();
        assert_eq!(
            extract_error_kind(&refusal).as_deref(),
            Some("invalid_configuration")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TRUNCATED);
    }
}
