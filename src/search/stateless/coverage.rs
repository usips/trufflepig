use crate::{results::ResultSet, search::Query};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(super) struct CurrentCoverage {
    pub walked_files: usize,
    pub read_files: usize,
    pub excluded_files: usize,
    pub read_failures: usize,
    pub walk_failures: usize,
    pub parse_failures: usize,
    pub unsearched_files: usize,
    pub unsearched_lower_bound: bool,
    pub unsearched_unknown: bool,
    pub path_catalog_truncated: bool,
    pub bytes_read: usize,
    pub facts_indexed: usize,
    pub complete: bool,
}

impl Default for CurrentCoverage {
    fn default() -> Self {
        Self {
            walked_files: 0,
            read_files: 0,
            excluded_files: 0,
            read_failures: 0,
            walk_failures: 0,
            parse_failures: 0,
            unsearched_files: 0,
            unsearched_lower_bound: false,
            unsearched_unknown: false,
            path_catalog_truncated: false,
            bytes_read: 0,
            facts_indexed: 0,
            complete: true,
        }
    }
}

impl CurrentCoverage {
    pub(super) fn value(&self) -> Value {
        json!({
            "mode": "stateless",
            "complete": self.complete,
            "walked_files": self.walked_files,
            "read_files": self.read_files,
            "excluded_files": self.excluded_files,
            "read_failures": self.read_failures,
            "walk_failures": self.walk_failures,
            "parse_failures": self.parse_failures,
            "unsearched_files": self.unsearched_files,
            "unsearched_lower_bound": self.unsearched_lower_bound,
            "unsearched_unknown": self.unsearched_unknown,
            "path_catalog_truncated": self.path_catalog_truncated,
            "bytes_read": self.bytes_read,
            "facts_indexed": self.facts_indexed,
        })
    }
}

pub(super) fn apply(
    set: &mut ResultSet,
    current: &CurrentCoverage,
    query: &Query,
    semantic_requested: bool,
    rerank_requested: bool,
) {
    let unsearched = current
        .unsearched_files
        .saturating_add(current.read_failures)
        .saturating_add(current.walk_failures)
        .saturating_add(current.parse_failures);
    let complete = current.complete && unsearched == 0 && !current.unsearched_unknown;
    set.generation = 0;
    set.coverage["endpoint"] = "stateless_current_source".into();
    set.coverage["indexed_files"] = current.read_files.into();
    set.coverage["total_files"] = current.walked_files.into();
    set.coverage["indexed_bytes"] = current.bytes_read.into();
    set.coverage["excluded_files"] = current.excluded_files.into();
    set.coverage["walk_failures"] = current.walk_failures.into();
    set.coverage["parse_failures"] = current.parse_failures.into();
    set.coverage["current_source"] = current.value();
    if unsearched > 0 {
        set.coverage["current_source_unsearched"] = unsearched.into();
    }
    if set.hits.is_empty() {
        set.coverage["absence"] = if complete { "exhaustive" } else { "not_proven" }.into();
    }
    set.truncated |= !complete || current.path_catalog_truncated;
    if query.semantic_eligible() || semantic_requested {
        let status = if query.semantic_eligible() {
            "unavailable"
        } else {
            "skipped"
        };
        set.coverage["semantic_status"] = status.into();
        set.coverage["semantic_reason"] = "stateless current-source mode".into();
    }
    if rerank_requested {
        let status = if query.semantic_eligible() {
            "unavailable"
        } else {
            "skipped"
        };
        set.coverage["rerank_status"] = status.into();
        set.coverage["rerank_reason"] = "stateless current-source mode".into();
    }
}
