//! Shorter cache-free coverage for pages whose full coverage does not fit the
//! budget: `Compact` keeps every field the coverage summary and follow-up read,
//! `Minimal` keeps completeness, absence, and the unsearched count.
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CoverageDetail {
    Full,
    Compact,
    Minimal,
}

const COMPACT_KEYS: [&str; 18] = [
    "complete",
    "absence",
    "indexed_files",
    "total_files",
    "excluded_files",
    "parse_failures",
    "walk_failures",
    "current_source_unsearched",
    "live_read_failures",
    "live_walk_failures",
    "semantic_status",
    "rerank_status",
    "filter_diagnosis",
    "reference_sites",
    "reference_files",
    "reference_sites_truncated",
    "hint",
    "next_step",
];
const MINIMAL_KEYS: [&str; 3] = ["complete", "absence", "current_source_unsearched"];
const CURRENT_SOURCE_KEYS: [&str; 4] = ["complete", "limit", "body_lane", "unsearched_unknown"];

/// `coverage` (one repository's object or workspace member rows) at `detail`.
pub(super) fn digest(coverage: &Value, detail: CoverageDetail) -> Value {
    match (coverage, detail) {
        (_, CoverageDetail::Full) => coverage.clone(),
        (Value::Array(rows), _) => {
            Value::Array(rows.iter().map(|row| member_row(row, detail)).collect())
        }
        (Value::Object(_), CoverageDetail::Compact) => {
            let mut object = pick(coverage, &COMPACT_KEYS);
            let current = pick(&coverage["current_source"], &CURRENT_SOURCE_KEYS);
            if !current.is_empty() {
                object.insert("current_source".into(), Value::Object(current));
            }
            Value::Object(object)
        }
        (Value::Object(_), CoverageDetail::Minimal) => Value::Object(pick(coverage, &MINIMAL_KEYS)),
        _ => coverage.clone(),
    }
}

/// One workspace member row with its nested coverage folded into a few fields.
fn member_row(row: &Value, detail: CoverageDetail) -> Value {
    let mut object = pick(row, &["member", "state", "hits", "worktree"]);
    let coverage = &row["coverage"];
    let mut copy = |key: &str, value: &Value| {
        if !value.is_null() {
            object.insert(key.into(), value.clone());
        }
    };
    copy("complete", &coverage["complete"]);
    if detail == CoverageDetail::Compact {
        copy("absence", &coverage["absence"]);
        copy("unsearched", &coverage["current_source_unsearched"]);
        copy("limit", &coverage["current_source"]["limit"]);
        copy("reason", &row["reason"]);
    }
    Value::Object(object)
}

fn pick(value: &Value, keys: &[&str]) -> Map<String, Value> {
    let mut picked = Map::new();
    for key in keys {
        if let Some(found) = value.get(*key) {
            picked.insert((*key).into(), found.clone());
        }
    }
    picked
}

/// The first hint and follow-up command in `coverage` or its member rows.
pub(super) fn follow_up(coverage: &Value) -> (Option<String>, Option<String>) {
    let source = match coverage {
        Value::Array(rows) => rows
            .iter()
            .map(|row| &row["coverage"])
            .find(|coverage| coverage.get("hint").is_some())
            .unwrap_or(&Value::Null),
        value => value,
    };
    let text = |key: &str| source[key].as_str().map(str::to_owned);
    (text("hint"), text("next_step"))
}
