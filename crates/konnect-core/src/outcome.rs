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
    "batch_place_components",
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
