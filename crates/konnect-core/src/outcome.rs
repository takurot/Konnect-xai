//! Shared, compatibility-preserving tool outcome metadata.
//!
//! Tool-specific response fields remain authoritative for their domain. This
//! envelope gives callers and observability one common answer to whether the
//! requested work completed, partially completed, failed before mutation, or
//! may have changed state before verification failed.

use crate::mcp::protocol::{CallToolResult, ToolContent};
use serde::Serialize;
use serde_json::{json, Value};

/// Tools that have adopted the shared outcome envelope.
///
/// Keep this deliberately bounded. Adding a tool is a contract migration: its
/// success, refusal, partial and post-mutation failure paths need evidence
/// before it belongs here.
pub const ADOPTED_OUTCOME_TOOLS: &[&str] = &[
    "add_schematic_component",
    "annotate_schematic",
    "batch_add_wire",
    "batch_connect_pins",
    "batch_connect_to_net",
    "batch_delete",
    "batch_delete_no_connect",
    "batch_delete_schematic_components",
    "batch_delete_schematic_wire",
    "batch_edit_schematic_components",
    "batch_place_components",
    "batch_rotate_labels",
    "bulk_move_schematic_components",
    "connect_pins_to_bus",
    "group_components",
    "run_design_review",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Complete,
    Partial,
    Failed,
    Uncertain,
}

impl OutcomeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
        }
    }
}

pub fn summary(
    status: OutcomeStatus,
    target: impl Into<String>,
    source: impl Into<String>,
    requested: usize,
    completed: usize,
    failed: usize,
    retry: Option<Value>,
) -> Value {
    debug_assert_eq!(
        requested,
        completed.saturating_add(failed),
        "outcome counts must account for every requested item or check"
    );
    json!({
        "status": status,
        "target": target.into(),
        "source": source.into(),
        "requested": requested,
        "completed": completed,
        "failed": failed,
        "retry": retry
    })
}

pub fn retry_failed_items(indexes: Vec<usize>) -> Value {
    json!({
        "safe": true,
        "scope": "failed_items",
        "item_indexes": indexes,
        "instruction": "correct and retry only the listed input items"
    })
}

pub fn retry_whole_request() -> Value {
    json!({
        "safe": true,
        "scope": "whole_request",
        "instruction": "correct the refusal and retry the request; no item was applied"
    })
}

pub fn inspect_before_retry() -> Value {
    json!({
        "safe": false,
        "scope": "inspect_target",
        "instruction": "reload and inspect the target before retrying; do not blindly repeat the mutation"
    })
}

/// Kind of a batch item whose own spec is malformed.
pub const INVALID_ITEM: &str = "invalid_item";
/// Kind of a batch item whose target is not in the document.
pub const NOT_FOUND: &str = "not_found";

/// One batch input item that was refused, by its index in the request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemFailure {
    pub index: usize,
    pub kind: String,
    pub message: String,
}

impl ItemFailure {
    pub fn new(index: usize, kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            index,
            kind: kind.into(),
            message: message.into(),
        }
    }

    /// The item refused by a single-item handler's error result.
    pub fn from_result(index: usize, result: &CallToolResult) -> Self {
        let text = match result.content.first() {
            Some(ToolContent::Text { text }) => text.as_str(),
            _ => "the item was refused",
        };
        let body = serde_json::from_str::<Value>(text).ok();
        let kind = body
            .as_ref()
            .and_then(|body| body.pointer("/error/kind"))
            .and_then(Value::as_str)
            .unwrap_or("handler_error");
        let message = body
            .as_ref()
            .and_then(|body| body["message"].as_str())
            .unwrap_or(text);
        Self::new(index, kind, message)
    }
}

/// What a batch call did with each input item.
///
/// `applied` holds the indexes whose effect is in the file afterwards,
/// including items that were already satisfied. `failures` holds the items at
/// fault. An all-or-nothing tool that refuses a batch applies nothing, so its
/// valid items appear in neither list.
#[derive(Debug, Clone, Default)]
pub struct BatchOutcome {
    pub requested: usize,
    pub applied: Vec<usize>,
    pub failures: Vec<ItemFailure>,
    pub written: bool,
}

impl BatchOutcome {
    pub fn new(requested: usize) -> Self {
        Self {
            requested,
            ..Self::default()
        }
    }

    pub fn status(&self) -> OutcomeStatus {
        if self.applied.len() == self.requested && self.failures.is_empty() {
            OutcomeStatus::Complete
        } else if self.applied.is_empty() {
            OutcomeStatus::Failed
        } else {
            OutcomeStatus::Partial
        }
    }

    fn to_json(&self, target: &str, source: &str) -> Value {
        let status = self.status();
        let retry = match status {
            OutcomeStatus::Complete => None,
            OutcomeStatus::Partial => Some(retry_failed_items(
                self.failures.iter().map(|failure| failure.index).collect(),
            )),
            OutcomeStatus::Failed | OutcomeStatus::Uncertain => Some(retry_whole_request()),
        };
        let mut outcome = summary(
            status,
            target,
            source,
            self.requested,
            self.applied.len(),
            self.requested.saturating_sub(self.applied.len()),
            retry,
        );
        outcome["applied"] = json!(self.applied);
        outcome["failures"] = json!(self.failures);
        outcome["written"] = json!(self.written);
        outcome
    }
}

/// A batch tool's response: `body` plus the shared `outcome`.
///
/// A batch in which nothing was applied is an error. Unless `body` already
/// carries a structured `error`, it gains a `batch_failed` one so the result
/// still has a stable kind.
pub fn batch_result(
    body: Value,
    outcome: &BatchOutcome,
    target: &std::path::Path,
    source: &str,
) -> CallToolResult {
    let mut body = match body {
        Value::Object(object) => object,
        other => serde_json::Map::from_iter([("result".to_string(), other)]),
    };
    let failed = outcome.status() == OutcomeStatus::Failed;
    if failed && !body.contains_key("error") {
        let detail = outcome
            .failures
            .iter()
            .map(|failure| format!("[{}] {}", failure.index, failure.message))
            .collect::<Vec<_>>()
            .join("; ");
        body.insert(
            "message".to_string(),
            json!(format!("no batch item was applied: {detail}")),
        );
        body.insert(
            "error".to_string(),
            json!(crate::mcp::error::ToolErrorKind::BatchFailed {
                failed: outcome.failures.len()
            }),
        );
    }
    body.insert(
        "outcome".to_string(),
        outcome.to_json(&target.display().to_string(), source),
    );
    let mut result = CallToolResult::json(&Value::Object(body));
    result.is_error = failed;
    result
}

/// A whole-batch refusal raised while planning item `failures`: nothing was
/// applied. A structured refusal keeps its own error kind.
pub fn refuse_batch(
    refusal: &CallToolResult,
    failures: Vec<ItemFailure>,
    requested: usize,
    target: &std::path::Path,
) -> CallToolResult {
    let body = match refusal.content.first() {
        Some(ToolContent::Text { text }) => serde_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({ "message": text })),
        _ => json!({ "message": "the batch was refused" }),
    };
    let outcome = BatchOutcome {
        failures,
        ..BatchOutcome::new(requested)
    };
    batch_result(body, &outcome, target, SAVED_FILE)
}

/// Source of an outcome whose write was revision-checked against the bytes
/// it was planned from.
pub const SAVED_FILE: &str = "saved_file";

/// Add the shared envelope while preserving every existing response field.
pub fn attach(mut result: CallToolResult, outcome: Value) -> CallToolResult {
    let Some(ToolContent::Text { text }) = result.content.first_mut() else {
        return result;
    };
    let Ok(mut body) = serde_json::from_str::<Value>(text) else {
        return result;
    };
    if let Some(object) = body.as_object_mut() {
        object.insert("outcome".to_string(), outcome);
        if let Ok(serialized) = serde_json::to_string(&body) {
            *text = serialized;
        }
    }
    result
}

pub fn status(result: &CallToolResult) -> Option<OutcomeStatus> {
    let ToolContent::Text { text } = result.content.first()? else {
        return None;
    };
    let body: Value = serde_json::from_str(text).ok()?;
    match body.pointer("/outcome/status")?.as_str()? {
        "complete" => Some(OutcomeStatus::Complete),
        "partial" => Some(OutcomeStatus::Partial),
        "failed" => Some(OutcomeStatus::Failed),
        "uncertain" => Some(OutcomeStatus::Uncertain),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    const LEGACY_BASELINE_CEILING: usize = 3;

    #[derive(Debug, Deserialize)]
    struct ReliabilityBaseline {
        schema_version: u32,
        adopted_outcome_tools: Vec<String>,
        legacy_entry_ceiling: usize,
        legacy_entries: Vec<LegacyEntry>,
    }

    #[derive(Debug, Deserialize)]
    struct LegacyEntry {
        id: String,
        paths: Vec<String>,
        rationale: String,
        tracking_issues: Vec<String>,
        removal_criteria: String,
    }

    fn is_tracking_issue_url(issue: &str) -> bool {
        use konnect_ipc::project_links::{REPO_URL, UPSTREAM_REPO_URL};
        [UPSTREAM_REPO_URL, REPO_URL].iter().any(|repo| {
            issue
                .strip_prefix(repo)
                .and_then(|rest| rest.strip_prefix("/issues/"))
                .is_some_and(|number| number.parse::<u64>().is_ok())
        })
    }

    #[test]
    fn tracking_issues_may_live_upstream_or_in_this_fork() {
        assert!(is_tracking_issue_url(
            "https://github.com/mixelpixx/Konnect/issues/119"
        ));
        assert!(is_tracking_issue_url(
            "https://github.com/takurot/Konnect-xai/issues/10"
        ));
        for invalid in [
            "https://github.com/takurot/Konnect-xai/issues/",
            "https://github.com/takurot/Konnect-xai/issues/ten",
            "https://github.com/takurot/Konnect-xai/pull/10",
            "https://github.com/someone/else/issues/10",
            "https://github.com/takurot/Konnect-xai/issues/x/10",
        ] {
            assert!(!is_tracking_issue_url(invalid), "{invalid}");
        }
    }

    #[test]
    fn reliability_contract_inventory_is_reviewed_and_bounded() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let inventory_path = root.join("docs/reliability-legacy-inventory.json");
        let bytes = std::fs::read(&inventory_path).unwrap_or_else(|error| {
            panic!(
                "reliability inventory {} must be tracked: {error}",
                inventory_path.display()
            )
        });
        let baseline: ReliabilityBaseline = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("{}: {error}", inventory_path.display()));

        assert_eq!(baseline.schema_version, 1, "unknown inventory schema");
        assert_eq!(
            baseline.legacy_entry_ceiling, LEGACY_BASELINE_CEILING,
            "raising tolerated legacy debt requires an explicit guard review"
        );
        assert_eq!(
            baseline.legacy_entries.len(),
            baseline.legacy_entry_ceiling,
            "the ceiling must ratchet down when legacy debt is removed; new debt must not be hidden below an old ceiling"
        );

        let mut adopted = baseline.adopted_outcome_tools;
        adopted.sort();
        let mut expected = ADOPTED_OUTCOME_TOOLS
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(
            adopted, expected,
            "inventory and the code-owned outcome catalogue drifted"
        );
        for tool in ADOPTED_OUTCOME_TOOLS {
            assert!(
                crate::router::registry::ALL_TOOLSETS.iter().any(|toolset| {
                    crate::router::registry::tools_for(toolset.name)
                        .is_some_and(|definitions| definitions.iter().any(|def| def.name == *tool))
                }),
                "adopted outcome tool is no longer served: {tool}"
            );
        }

        let mut ids = std::collections::HashSet::new();
        for entry in baseline.legacy_entries {
            assert!(!entry.id.trim().is_empty(), "legacy entry needs an id");
            assert!(ids.insert(entry.id.clone()), "duplicate id: {}", entry.id);
            assert!(
                !entry.paths.is_empty(),
                "{} needs at least one path",
                entry.id
            );
            assert!(
                !entry.rationale.trim().is_empty(),
                "{} needs a rationale",
                entry.id
            );
            assert!(
                !entry.removal_criteria.trim().is_empty(),
                "{} needs removal criteria",
                entry.id
            );
            assert!(
                !entry.tracking_issues.is_empty(),
                "{} needs a tracking issue",
                entry.id
            );
            for issue in entry.tracking_issues {
                assert!(
                    is_tracking_issue_url(&issue),
                    "{} has an invalid issue URL: {issue}",
                    entry.id
                );
            }
            for path in entry.paths {
                assert!(
                    root.join(&path).is_file(),
                    "{} names a stale or missing path: {path}",
                    entry.id
                );
            }
        }
    }

    /// Served batch tools that have not adopted the batch outcome, and why.
    const BATCH_TOOLS_WITHOUT_OUTCOME: &[(&str, &str)] = &[
        (
            "batch_add_no_connect",
            "not listed in #133; keeps its per-position errors from #132",
        ),
        (
            "batch_add_junction",
            "not listed in #133; refuses the whole batch with invalid_argument",
        ),
        ("batch_add_bus", "not listed in #133"),
        ("batch_get_schematic_pin_locations", "read-only"),
    ];

    #[test]
    fn every_served_schematic_batch_tool_adopts_the_outcome_or_says_why_not() {
        let batch_like = |name: &str| {
            name.starts_with("batch_")
                || name.starts_with("bulk_")
                || matches!(name, "group_components" | "connect_pins_to_bus")
        };
        for toolset in crate::router::registry::ALL_TOOLSETS {
            if !toolset.name.starts_with("sch_") {
                continue;
            }
            for definition in crate::router::registry::tools_for(toolset.name).unwrap_or_default() {
                let name = definition.name;
                if !batch_like(name) {
                    continue;
                }
                let exempt = BATCH_TOOLS_WITHOUT_OUTCOME
                    .iter()
                    .any(|(exempt, _)| *exempt == name);
                assert!(
                    ADOPTED_OUTCOME_TOOLS.contains(&name) != exempt,
                    "{name} must either adopt the batch outcome or be listed with a reason"
                );
            }
        }
    }

    #[test]
    fn batch_status_follows_applied_and_failed_items() {
        let failure = |index| ItemFailure::new(index, NOT_FOUND, "missing");
        let cases = [
            (BatchOutcome::new(0), OutcomeStatus::Complete),
            (
                BatchOutcome {
                    applied: vec![0, 1],
                    ..BatchOutcome::new(2)
                },
                OutcomeStatus::Complete,
            ),
            (
                BatchOutcome {
                    applied: vec![0],
                    failures: vec![failure(1)],
                    ..BatchOutcome::new(2)
                },
                OutcomeStatus::Partial,
            ),
            (
                BatchOutcome {
                    applied: vec![0, 1],
                    failures: vec![failure(1)],
                    ..BatchOutcome::new(2)
                },
                OutcomeStatus::Partial,
            ),
            (
                BatchOutcome {
                    failures: vec![failure(1)],
                    ..BatchOutcome::new(2)
                },
                OutcomeStatus::Failed,
            ),
        ];
        for (outcome, expected) in cases {
            assert_eq!(outcome.status(), expected, "{outcome:?}");
        }
    }

    #[test]
    fn failed_batch_is_an_error_with_a_kind_unless_the_refusal_has_one() {
        let path = std::path::Path::new("s.kicad_sch");
        let outcome = BatchOutcome {
            failures: vec![ItemFailure::new(0, NOT_FOUND, "Component 'R99' not found")],
            ..BatchOutcome::new(1)
        };
        let result = batch_result(json!({"errors": []}), &outcome, path, SAVED_FILE);
        assert!(result.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("batch_failed")
        );
        assert_eq!(status(&result), Some(OutcomeStatus::Failed));

        let refusal = crate::mcp::protocol::CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "wires[1]".into(),
                reason: "missing".into(),
            },
            "bad",
        );
        let refused = refuse_batch(
            &refusal,
            vec![ItemFailure::from_result(1, &refusal)],
            2,
            path,
        );
        assert_eq!(
            crate::mcp::error::extract_error_kind(&refused).as_deref(),
            Some("invalid_argument")
        );
        let ToolContent::Text { text } = &refused.content[0] else {
            panic!("expected text")
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["outcome"]["failures"][0]["kind"], "invalid_argument");
        assert_eq!(body["outcome"]["failures"][0]["index"], 1);
        assert_eq!(body["outcome"]["failed"], 2);
        assert_eq!(body["outcome"]["retry"]["scope"], "whole_request");

        let plain = refuse_batch(
            &crate::mcp::protocol::CallToolResult::error("plain refusal"),
            Vec::new(),
            1,
            path,
        );
        assert_eq!(
            crate::mcp::error::extract_error_kind(&plain).as_deref(),
            Some("batch_failed")
        );
    }

    #[test]
    fn attach_preserves_legacy_fields_and_exposes_status() {
        let result = attach(
            CallToolResult::json(&json!({"placed_count": 2})),
            summary(
                OutcomeStatus::Partial,
                "board.kicad_sch",
                "saved_file_readback",
                3,
                2,
                1,
                Some(retry_failed_items(vec![1])),
            ),
        );
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text")
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["placed_count"], 2);
        assert_eq!(body["outcome"]["status"], "partial");
        assert_eq!(status(&result), Some(OutcomeStatus::Partial));
    }
}

#[cfg(test)]
mod writer_ratchet_tests {
    #[test]
    fn reliability_contract_unchecked_writer_inventory() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let baseline: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("docs/reliability-legacy-inventory.json")).unwrap(),
        )
        .unwrap();
        let allowed = baseline["unchecked_write_atomic"]
            .as_array()
            .expect("reviewed unchecked writer inventory");
        fn visit(path: &std::path::Path, root: &std::path::Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, root, found);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let count = text.matches("write_atomic(").count();
                    if count > 0 {
                        found.push(format!(
                            "{}:{count}",
                            path.strip_prefix(root)
                                .unwrap()
                                .to_string_lossy()
                                .replace('\\', "/")
                        ));
                    }
                }
            }
        }
        let mut actual = Vec::new();
        visit(
            &root.join("crates/konnect-core/src/tools"),
            &root,
            &mut actual,
        );
        actual.sort();
        let mut expected = allowed
            .iter()
            .map(|x| x["site"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(actual, expected);
    }
}

#[cfg(test)]
mod outcome_boundary_contract_tests {
    use super::*;

    #[test]
    fn outcome_attachment_preserves_non_object_and_non_text_responses() {
        let mut cases = vec![
            CallToolResult {
                content: Vec::new(),
                is_error: true,
            },
            CallToolResult {
                content: vec![ToolContent::Image {
                    data: "image".into(),
                    mime_type: "image/png".into(),
                }],
                is_error: false,
            },
        ];
        cases.extend(
            ["ordinary text", "null", "[]", "42", r#""string""#]
                .into_iter()
                .map(CallToolResult::text),
        );
        for response in cases {
            let before = serde_json::to_value(&response).unwrap();
            let after = attach(response, json!({"status":"complete"}));
            assert_eq!(serde_json::to_value(&after).unwrap(), before);
            assert_eq!(status(&after), None);
        }
    }

    #[test]
    fn unknown_or_non_string_outcome_status_is_never_promoted_to_complete() {
        for invalid in [json!("success"), json!(null), json!(true), json!(7)] {
            let response = CallToolResult::json(&json!({"outcome":{"status":invalid}}));
            assert_eq!(status(&response), None);
        }
        for (value, expected) in [
            ("complete", OutcomeStatus::Complete),
            ("partial", OutcomeStatus::Partial),
            ("failed", OutcomeStatus::Failed),
            ("uncertain", OutcomeStatus::Uncertain),
        ] {
            assert_eq!(
                status(&CallToolResult::json(&json!({"outcome":{"status":value}}))),
                Some(expected)
            );
        }
    }
}
