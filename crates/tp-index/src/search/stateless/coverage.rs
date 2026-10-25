//! Cache-free coverage: what the bounded walk and reads searched, why they
//! stopped early, and the scoped follow-up command an incomplete page suggests.
use crate::{results::ResultSet, search::Query};
use serde_json::{Value, json};

/// Why a scan stopped before searching every eligible file.
pub(super) const LIMIT_DEADLINE: &str = "deadline";
pub(super) const LIMIT_FILES: &str = "file_cap";
pub(super) const LIMIT_BYTES: &str = "byte_cap";
pub(super) const LIMIT_FACTS: &str = "fact_cap";
pub(super) const LIMIT_WALK: &str = "walk_cap";
pub(super) const LIMIT_BODIES_SKIPPED: &str = "path_matches_first";

#[derive(Clone, Debug)]
pub(super) struct CurrentCoverage {
    pub walked_files: usize,
    pub read_files: usize,
    /// Files read and ruled out by the literal prefilter without extraction.
    pub prefiltered_files: usize,
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
    /// The first limit that stopped the scan, if any.
    pub limit: Option<&'static str>,
    /// `kind:file` body lane: `complete`, `partial`, or `skipped`.
    pub body_lane: Option<&'static str>,
    /// The first eligible path left unsearched, which scopes the follow-up.
    pub first_unsearched: Option<String>,
}

impl Default for CurrentCoverage {
    fn default() -> Self {
        Self {
            walked_files: 0,
            read_files: 0,
            prefiltered_files: 0,
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
            limit: None,
            body_lane: None,
            first_unsearched: None,
        }
    }
}

impl CurrentCoverage {
    /// Records that `unsearched` eligible files, starting at `first`, were not searched.
    pub(super) fn stop(&mut self, limit: &'static str, unsearched: usize, first: Option<&str>) {
        self.complete = false;
        self.unsearched_files = self.unsearched_files.saturating_add(unsearched);
        self.limit.get_or_insert(limit);
        if self.first_unsearched.is_none() {
            self.first_unsearched = first.map(str::to_owned);
        }
    }

    pub(super) fn value(&self) -> Value {
        let mut value = json!({
            "mode": "stateless",
            "complete": self.complete,
            "walked_files": self.walked_files,
            "read_files": self.read_files,
            "prefiltered_files": self.prefiltered_files,
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
        });
        if let Some(limit) = self.limit {
            value["limit"] = limit.into();
        }
        if let Some(lane) = self.body_lane {
            value["body_lane"] = lane.into();
        }
        value
    }
}

/// The command an incomplete page suggests next.
#[derive(Clone, Copy)]
pub(super) enum FollowUp<'a> {
    Search(&'a Query),
    Refs(&'a Query),
    Map,
}

pub(super) fn apply(
    set: &mut ResultSet,
    current: &CurrentCoverage,
    follow_up: FollowUp<'_>,
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
    set.coverage["complete"] = complete.into();
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
    if let Some((hint, next)) = follow_up_hint(current, follow_up) {
        set.coverage["hint"] = hint.into();
        if let Some(next) = next {
            set.coverage["next_step"] = next.into();
        }
    }
    let query = match follow_up {
        FollowUp::Search(query) => Some(query),
        FollowUp::Refs(_) | FollowUp::Map => None,
    };
    let eligible = query.is_some_and(Query::semantic_eligible);
    let status = if eligible { "unavailable" } else { "skipped" };
    if eligible || semantic_requested {
        set.coverage["semantic_status"] = status.into();
        set.coverage["semantic_reason"] = "stateless current-source mode".into();
    }
    if rerank_requested {
        set.coverage["rerank_status"] = status.into();
        set.coverage["rerank_reason"] = "stateless current-source mode".into();
    }
}

/// A one-line explanation of a scan limit, plus a narrower command when the
/// first unsearched path names a directory.
fn follow_up_hint(
    current: &CurrentCoverage,
    follow_up: FollowUp<'_>,
) -> Option<(String, Option<String>)> {
    let limit = current.limit?;
    let searched = current.read_files.saturating_add(current.prefiltered_files);
    let unsearched = if current.unsearched_lower_bound {
        format!("{}+", current.unsearched_files)
    } else {
        current.unsearched_files.to_string()
    };
    let hint = match (limit, follow_up) {
        (LIMIT_BODIES_SKIPPED, _) => {
            "path matches filled the page, so file bodies were not searched; \
             use re:TEXT or drop kind:file for body matches"
                .to_owned()
        }
        (_, FollowUp::Map) => format!(
            "{limit}: outlined {} files, {unsearched} not outlined; map a subdirectory",
            current.read_files
        ),
        _ if current.path_catalog_truncated => format!(
            "{limit}: the file catalog stopped early; retry, or search from a narrower --root"
        ),
        _ => format!(
            "{limit}: searched {searched} files, {unsearched} unsearched; narrow with file:PREFIX"
        ),
    };
    let parent = current
        .first_unsearched
        .as_deref()
        .and_then(|path| path.rsplit_once('/'))
        .map(|(directory, _)| directory);
    let next = parent.and_then(|parent| match follow_up {
        FollowUp::Map => Some(format!("map {parent}")),
        _ if current.path_catalog_truncated => None,
        FollowUp::Search(query) => {
            let scope = narrower_scope(parent, query);
            Some(format!("search '{}'", scoped_query(query, scope)))
        }
        FollowUp::Refs(query) => {
            let scope = narrower_scope(parent, query);
            Some(format!("refs {}", scoped_query(query, scope)))
        }
    });
    Some((hint, next))
}

/// `parent` cut one component below the query's deepest `file:` value (two
/// components without one), so the follow-up narrows the original scope.
fn narrower_scope<'a>(parent: &'a str, query: &Query) -> &'a str {
    let depth = query
        .path
        .includes()
        .iter()
        .map(|value| value.trim_matches('/').split('/').count() + 1)
        .max()
        .unwrap_or(2);
    match parent.match_indices('/').nth(depth.saturating_sub(1)) {
        Some((cut, _)) => &parent[..cut],
        None => parent,
    }
}

/// `query` rewritten with its `file:` values replaced by `directory/`.
fn scoped_query(query: &Query, directory: &str) -> String {
    let mut words = Vec::with_capacity(4 + query.path.excludes().len());
    let prefix = if query.exact {
        "sym:"
    } else if query.regex {
        "re:"
    } else {
        ""
    };
    if !query.text.is_empty() {
        words.push(format!("{prefix}{}", query.text));
    }
    if !query.language.is_empty() {
        words.push(format!("lang:{}", query.language));
    }
    if !query.kind.is_empty() {
        words.push(format!("kind:{}", query.kind));
    }
    for exclude in query.path.excludes() {
        if exclude != ".git" {
            words.push(format!("-file:{exclude}"));
        }
    }
    words.push(format!("file:{directory}/"));
    words.join(" ")
}
