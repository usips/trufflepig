//! Workspace coverage in both page formats: the JSON whitelist and the
//! one-line member summary. The summary keeps every member's partial and
//! truncated state visible; only detail reasons are folded away.

use crate::results::lines::unconfigured;
use serde_json::Value;

/// Ranks retrieval-lane statuses so the summary can report the worst member.
fn status_rank(status: &str) -> u8 {
    match status {
        "ready" => 0,
        "pending" => 1,
        "partial" => 2,
        _ => 3,
    }
}

/// `MEMBER complete|partial (N unsearched)[ truncated]; ...; semantic STATUS;
/// rerank unavailable`. Lanes that were never configured are omitted.
pub(crate) fn member_coverage_summary(values: &[Value]) -> String {
    let mut parts = Vec::with_capacity(values.len() + 2);
    let mut semantic: Option<&str> = None;
    let mut rerank_unavailable = false;
    for value in values {
        let member = value["member"].as_str().unwrap_or("?");
        let name = match value["worktree"].as_str() {
            Some(label) => format!("{member}@{label}"),
            None => member.to_owned(),
        };
        let state = value["state"].as_str().unwrap_or("unknown");
        let mut part = if state == "searched" {
            match value["unsearched"].as_u64() {
                _ if !value["partial"].as_bool().unwrap_or(false) => format!("{name} complete"),
                Some(count) if count > 0 => format!("{name} partial ({count} unsearched)"),
                _ => format!("{name} partial"),
            }
        } else {
            format!("{name} {state}")
        };
        if value["truncated"].as_bool().unwrap_or(false) {
            part.push_str(" truncated");
        }
        parts.push(part);
        let issues = &value["issues"];
        if let Some(status) = issues["semantic_status"].as_str()
            && !unconfigured(issues["semantic_reason"].as_str())
            && semantic.is_none_or(|current| status_rank(status) > status_rank(current))
        {
            semantic = Some(status);
        }
        rerank_unavailable |= issues["rerank_status"].as_str() == Some("unavailable")
            && !unconfigured(issues["rerank_reason"].as_str());
    }
    if let Some(status) = semantic
        && status != "ready"
    {
        parts.push(format!("semantic {status}"));
    }
    if rerank_unavailable {
        parts.push("rerank unavailable".to_owned());
    }
    parts.join("; ")
}

/// Whitelists the coverage keys a JSON page carries and shortens long reasons.
pub(crate) fn compact_coverage(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .map(|value| {
            let Some(object) = value.as_object() else {
                return value.clone();
            };
            let mut compact = serde_json::Map::new();
            for key in [
                "member",
                "root",
                "worktree",
                "state",
                "status",
                "available",
                "availability",
                "pending",
                "pending_reason",
                "pending_status",
                "reason",
                "reasons",
                "unavailable_reason",
                "error",
                "failure",
                "generation",
                "retained",
                "truncated",
                "partial",
                "unsearched",
                "issues",
            ] {
                if let Some(value) = object.get(key) {
                    compact.insert(key.into(), value.clone());
                }
            }
            if let Some(issues) = compact.get_mut("issues").and_then(Value::as_object_mut) {
                if issues.get("semantic_status").and_then(Value::as_str) == Some("ready") {
                    for key in [
                        "semantic_scope",
                        "semantic_regions",
                        "semantic_total_regions",
                    ] {
                        issues.remove(key);
                    }
                }
                for key in ["semantic_reason", "semantic_preparation_error"] {
                    if let Some(reason) = issues.get(key).and_then(Value::as_str) {
                        let mut short: String = reason.chars().take(120).collect();
                        if reason.chars().count() > 120 {
                            short.push('…');
                        }
                        issues.insert(key.into(), short.into());
                    }
                }
            }
            Value::Object(compact)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn member_summary_keeps_partial_and_truncated_per_member() {
        let coverage = [
            json!({"member":"lunatic","state":"searched","partial":true,"truncated":false,
                   "issues":{"semantic_status":"partial","rerank_status":"ready"}}),
            json!({"member":"tg","state":"searched","partial":false,"truncated":true,
                   "issues":{"semantic_status":"unavailable","rerank_status":"unavailable"}}),
            json!({"member":"warm","state":"warming"}),
        ];
        assert_eq!(
            member_coverage_summary(&coverage),
            "lunatic partial; tg complete truncated; warm warming; semantic unavailable; rerank unavailable"
        );
        let ready = [json!({"member":"a","state":"searched","partial":false,
                            "issues":{"semantic_status":"ready"}})];
        assert_eq!(member_coverage_summary(&ready), "a complete");
    }

    #[test]
    fn member_summary_counts_unsearched_files_and_omits_unconfigured_lanes() {
        let coverage = [
            json!({"member":"a","state":"searched","partial":true,"unsearched":3,
                   "issues":{"semantic_status":"unavailable",
                             "semantic_reason":"semantic_unavailable: set model_dir in inference.toml",
                             "rerank_status":"unavailable",
                             "rerank_reason":"semantic_worker: model directory is not configured"}}),
            json!({"member":"b","state":"searched","partial":false,"unsearched":0,
                   "issues":{"excluded_files":40,"parse_failures":2}}),
        ];
        assert_eq!(
            member_coverage_summary(&coverage),
            "a partial (3 unsearched); b complete"
        );
    }

    #[test]
    fn member_summary_labels_substituted_worktree_root() {
        let coverage = [
            json!({"member":"lunatic","worktree":"feature-x","root":"/tmp/x","state":"searched","partial":false}),
            json!({"member":"tg","state":"warming"}),
        ];
        assert_eq!(
            member_coverage_summary(&coverage),
            "lunatic@feature-x complete; tg warming"
        );
    }
}
