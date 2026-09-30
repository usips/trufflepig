//! Workspace coverage in both page formats: the JSON whitelist and the
//! one-line member summary. The summary keeps every member's partial, truncated,
//! fallback and failure state visible; only detail reasons are folded away.

use super::OwnedEntry;
use crate::results::{ResultEntry, lines::unconfigured};
use serde_json::Value;
use std::collections::HashSet;

/// Ranks retrieval-lane statuses so the summary can report the worst member.
fn status_rank(status: &str) -> u8 {
    match status {
        "ready" => 0,
        "pending" => 1,
        "partial" => 2,
        _ => 3,
    }
}

/// `MEMBER complete|partial (N unsearched: KIND)[ truncated]; ...; semantic
/// STATUS; rerank unavailable`. A worktree answered by its parent reads
/// `MEMBER@WT warming → served from MEMBER index (N files differ)`; warming and
/// unavailable members add their reason. Never-configured lanes are omitted.
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
        let mut part = match state {
            "searched" if !value["partial"].as_bool().unwrap_or(false) => {
                format!("{name} complete")
            }
            "searched" => format!("{name} {}", partial_text(value)),
            "parent_fallback" => {
                let mut part = format!(
                    "{name} {} → served from {} ({})",
                    value["home_state"].as_str().unwrap_or("warming"),
                    value["served_from"].as_str().unwrap_or("parent index"),
                    differs_text(&value["differs"])
                );
                if value["partial"].as_bool().unwrap_or(false) {
                    part.push(' ');
                    part.push_str(&partial_text(value));
                }
                part
            }
            "timed_out" => format!("{name} timed out"),
            _ => match value["reason"].as_str() {
                Some(reason) => format!("{name} {state} ({reason})"),
                None => format!("{name} {state}"),
            },
        };
        if value["truncated"].as_bool().unwrap_or(false) {
            part.push_str(" truncated");
        }
        if let Some(refs) = crate::search::reference_summary(value) {
            part.push_str(&format!(" ({refs})"));
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

/// `partial (N unsearched: KIND[, KIND])`, naming why files went unsearched.
fn partial_text(value: &Value) -> String {
    let kinds: Vec<&str> = value["unsearched_kinds"]
        .as_array()
        .map(|kinds| kinds.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match value["unsearched"].as_u64() {
        Some(count) if count > 0 && !kinds.is_empty() => {
            format!("partial ({count} unsearched: {})", kinds.join(", "))
        }
        Some(count) if count > 0 => format!("partial ({count} unsearched)"),
        _ => "partial".to_owned(),
    }
}

/// `N files differ`, `no files differ`, or `differences unknown` (null count).
fn differs_text(differs: &Value) -> String {
    match differs.as_u64() {
        None => "differences unknown".to_owned(),
        Some(0) => "no files differ".to_owned(),
        Some(1) => "1 file differs".to_owned(),
        Some(count) => format!("{count} files differ"),
    }
}

/// Appends `<TAB>differs` to the line of each hit whose file may differ in the
/// worktree from the parent index that answered.
pub(crate) fn mark_worktree_differs(text: String, hits: &[OwnedEntry]) -> String {
    let differing: HashSet<&str> = hits
        .iter()
        .filter(|owned| owned.worktree_differs)
        .filter_map(|owned| match &owned.entry {
            ResultEntry::LiveSource(hit) => Some(hit.handle.as_str()),
            _ => None,
        })
        .collect();
    if differing.is_empty() {
        return text;
    }
    let mut marked = String::with_capacity(text.len() + differing.len() * 8);
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        marked.push_str(body);
        if body
            .split_once('\t')
            .is_some_and(|(handle, _)| differing.contains(handle))
        {
            marked.push_str("\tdiffers");
        }
        if body.len() < line.len() {
            marked.push('\n');
        }
    }
    marked
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
                "home_state",
                "served_from",
                "differs",
                "differing_hits",
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
                "unsearched_kinds",
                "issues",
            ]
            .into_iter()
            .chain(crate::search::REFERENCE_COVERAGE_KEYS)
            {
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
mod tests;
