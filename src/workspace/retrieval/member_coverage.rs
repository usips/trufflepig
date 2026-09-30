//! One member's coverage row: what was searched, what could not be, and why.
use crate::{
    store::{Store, encode_path},
    workspace::member_root::MemberRoot,
};
use anyhow::Result;
use serde_json::{Value, json};

/// Counters of files a search could not examine, with the kind each reports.
/// Excluded (binary, oversized) files, parse failures (still lexically
/// searchable), and semantic lane status do not make lexical coverage partial;
/// they stay in `issues`.
const UNSEARCHED: [(&str, &str); 4] = [
    ("walk_failures", "walk_error"),
    ("truncated_files", "fact_limit"),
    ("live_read_failures", "read_error"),
    ("live_walk_failures", "walk_error"),
];
/// Paths listed for unsearched files with a recorded index status.
const UNSEARCHED_PATH_LIMIT: i64 = 5;
/// Longest failure reason a coverage row keeps.
const REASON_CHARS: usize = 120;

fn count(coverage: &Value, key: &str) -> u64 {
    coverage[key].as_u64().unwrap_or(0)
}

pub(super) fn partial_coverage(coverage: &Value) -> bool {
    UNSEARCHED.iter().any(|(key, _)| count(coverage, key) > 0)
}

/// Files a search could not examine, for the `partial (N unsearched)` summary.
pub(super) fn unsearched_files(coverage: &Value) -> u64 {
    UNSEARCHED.iter().map(|(key, _)| count(coverage, key)).sum()
}

/// Distinct kinds of unsearched files, for `partial (N unsearched: KIND)`.
pub(super) fn unsearched_kinds(coverage: &Value) -> Vec<&'static str> {
    let mut kinds = Vec::with_capacity(UNSEARCHED.len());
    for (key, kind) in UNSEARCHED {
        if count(coverage, key) > 0 && !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// Up to five indexed paths whose facts were dropped at the fact limit.
pub(super) fn fact_limit_paths(store: &Store) -> Result<Vec<String>> {
    let mut statement = store
        .conn
        .prepare("SELECT path FROM files WHERE status='fact_limit' ORDER BY path LIMIT ?1")?;
    let paths = statement
        .query_map([UNSEARCHED_PATH_LIMIT], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(paths)
}

pub(super) fn coverage_issues(coverage: &Value) -> Value {
    let mut issues = serde_json::Map::new();
    for key in [
        "parse_failures",
        "excluded_files",
        "walk_failures",
        "truncated_files",
        "live_read_failures",
        "live_walk_failures",
        "live_excluded_files",
        "semantic_failures",
        "semantic_pending",
    ] {
        if count(coverage, key) > 0 {
            issues.insert(key.into(), coverage[key].clone());
        }
    }
    for key in [
        "unsearched_paths",
        "semantic_status",
        "semantic_reason",
        "semantic_preparation_error",
        "rerank_status",
        "rerank_reason",
    ] {
        if let Some(value) = coverage.get(key) {
            issues.insert(key.into(), value.clone());
        }
    }
    if coverage.get("semantic_total_regions").is_some() {
        for key in [
            "semantic_total_regions",
            "semantic_regions",
            "semantic_scope",
        ] {
            issues.insert(key.into(), coverage[key].clone());
        }
    }
    Value::Object(issues)
}

/// `{"member","state"}` plus the substituted `root` and `worktree` label.
pub(super) fn member_row(member: &MemberRoot, state: &str) -> Value {
    let mut row = json!({"member":member.name(),"state":state});
    if let Some(label) = &member.worktree {
        row["root"] = encode_path(&member.root).into();
        row["worktree"] = label.clone().into();
    }
    row
}

/// A searched member's row from its answer's coverage.
pub(super) fn searched_row(
    member: &MemberRoot,
    generation: i64,
    retained: usize,
    truncated: bool,
    coverage: &Value,
) -> Value {
    let mut row = member_row(member, "searched");
    row["generation"] = generation.into();
    row["retained"] = retained.into();
    row["truncated"] = truncated.into();
    row["partial"] = partial_coverage(coverage).into();
    row["unsearched"] = unsearched_files(coverage).into();
    let kinds = unsearched_kinds(coverage);
    if !kinds.is_empty() {
        row["unsearched_kinds"] = kinds.into();
    }
    row["issues"] = coverage_issues(coverage);
    row
}

/// The first [`REASON_CHARS`] characters of a failure, ellipsized.
pub(super) fn short_reason(error: &anyhow::Error) -> String {
    let full = format!("{error:#}");
    let mut short: String = full.chars().take(REASON_CHARS).collect();
    if full.chars().count() > REASON_CHARS {
        short.push('…');
    }
    short
}
