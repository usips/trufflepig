//! `lines` rendering of result pages: one tab-separated line per entry, a
//! one-line coverage summary, then the shared footer. The budget measures this
//! text, so a lines page carries more hits than the JSON page for the same limit.

use super::{HitDetail, ResultEntry, concise_name};
use crate::output::lines::{COVERAGE_KEY, footer};
use serde_json::Value;
use std::fmt::Write;

/// Renders one entry: `HANDLE<TAB>[MEMBER/]PATH:START-END[<TAB>NAME]` for live
/// hits, followed by an indented `  LINE: TEXT` snippet line when present,
/// `HANDLE<TAB>commit:OID<TAB>SUMMARY` and
/// `HANDLE<TAB>change:STATUS<TAB>[MEMBER/]PATH<TAB>NAME` for history entries.
pub(crate) fn entry_line(entry: &ResultEntry, member: Option<&str>, detail: HitDetail) -> String {
    let names = detail.names;
    let prefix = member
        .map(|member| format!("{member}/"))
        .unwrap_or_default();
    let mut line = String::with_capacity(160);
    match entry {
        ResultEntry::LiveSource(hit) => {
            let _ = write!(
                line,
                "{}\t{prefix}{}:{}-{}",
                hit.handle, hit.path, hit.start_line, hit.end_line
            );
            // File-first regions are named after their path, and a regex hit's name
            // is the matched text its snippet already shows; repeating either is noise.
            let previewed = detail.snippets
                && hit.snippet.is_some()
                && hit.provenance.as_deref() == Some("live_regex");
            if names && concise_name(&hit.name) && hit.name != hit.path && !previewed {
                line.push('\t');
                line.push_str(&hit.name);
            }
            if let Some(resolution) = &hit.resolution {
                let _ = write!(line, "\tresolution={resolution}");
            }
            if detail.snippets
                && let Some(snippet) = &hit.snippet
            {
                let _ = write!(line, "\n  {}: {}", snippet.line, snippet.text);
            }
        }
        ResultEntry::Commit(commit) => {
            let summary = commit.summary.lines().next().unwrap_or_default();
            let _ = write!(
                line,
                "{}\tcommit:{}\t{summary}",
                commit.handle, commit.commit
            );
        }
        ResultEntry::Change(change) => {
            let path = change
                .after
                .as_ref()
                .or(change.before.as_ref())
                .map(|source| source.path.as_str())
                .unwrap_or_default();
            let _ = write!(
                line,
                "{}\tchange:{}\t{prefix}{path}\t{}",
                change.handle, change.status, change.name
            );
        }
    }
    line
}

/// Summarizes a single repository's coverage: indexed ratio, nonzero issue
/// counters, and any retrieval lane that is not `ready`.
pub(crate) fn single_repo_coverage(coverage: &Value) -> String {
    let count = |key: &str| coverage[key].as_u64().unwrap_or(0);
    let mut parts = vec![format!(
        "indexed {}/{}",
        count("indexed_files"),
        count("total_files")
    )];
    for key in [
        "excluded_files",
        "parse_failures",
        "walk_failures",
        "truncated_files",
    ] {
        let value = count(key);
        if value > 0 {
            parts.push(format!("{} {value}", key.trim_end_matches("_files")));
        }
    }
    for (label, key) in [("semantic", "semantic_status"), ("rerank", "rerank_status")] {
        if let Some(status) = coverage[key].as_str()
            && status != "ready"
        {
            parts.push(format!("{label} {status}"));
        }
    }
    if coverage.get("semantic_preparation_error").is_some() {
        parts.push("semantic preparation_error".to_owned());
    }
    if let Some(endpoint) = coverage["endpoint"].as_str() {
        parts.push(format!("endpoint {endpoint}"));
    }
    parts.join("; ")
}

/// Renders a page: the entries, the coverage line, then the footer.
pub(crate) fn page_text<'a>(
    entries: impl ExactSizeIterator<Item = (Option<&'a str>, &'a ResultEntry)>,
    detail: HitDetail,
    coverage: &str,
    next: Option<&str>,
    truncated: bool,
) -> String {
    let mut out = String::with_capacity(entries.len() * 160 + coverage.len() + 64);
    for (member, entry) in entries {
        out.push_str(&entry_line(entry, member, detail));
        out.push('\n');
    }
    out.push_str(COVERAGE_KEY);
    out.push_str(coverage);
    out.push('\n');
    footer(next, truncated, &mut out);
    out
}

/// Cursor for the entries after this page, `SET@OFFSET`.
pub(crate) fn next_cursor(id: &str, offset: usize, count: usize, total: usize) -> Option<String> {
    (offset + count < total).then(|| format!("{id}@{}", offset + count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::Hit;
    use serde_json::json;

    fn hit(name: &str) -> ResultEntry {
        ResultEntry::LiveSource(Hit {
            handle: "00000000000000000000000000000000:1".into(),
            path: "src/a%20b.rs".into(),
            revision: None,
            start: 0,
            end: 10,
            start_line: 3,
            end_line: 9,
            name: name.into(),
            kind: "function".into(),
            container: None,
            provenance: None,
            resolution: None,
            candidates: vec![],
            target: None,
            snippet: Some(crate::results::Snippet {
                line: 4,
                text: "fn run() {".into(),
            }),
        })
    }

    fn named(names: bool) -> HitDetail {
        HitDetail {
            names,
            snippets: false,
        }
    }

    #[test]
    fn live_line_carries_member_prefix_and_optional_name() {
        assert_eq!(
            entry_line(&hit("run"), Some("lunatic"), named(true)),
            "00000000000000000000000000000000:1\tlunatic/src/a%20b.rs:3-9\trun"
        );
        assert_eq!(
            entry_line(&hit(""), None, named(true)),
            "00000000000000000000000000000000:1\tsrc/a%20b.rs:3-9"
        );
        assert_eq!(
            entry_line(&hit("run"), None, named(false)),
            "00000000000000000000000000000000:1\tsrc/a%20b.rs:3-9"
        );
        assert_eq!(
            entry_line(&hit("src/a%20b.rs"), None, named(true)),
            "00000000000000000000000000000000:1\tsrc/a%20b.rs:3-9"
        );
    }

    #[test]
    fn snippet_follows_its_hit_on_an_indented_line() {
        let detail = HitDetail {
            names: true,
            snippets: true,
        };
        assert_eq!(
            entry_line(&hit("run"), None, detail),
            "00000000000000000000000000000000:1\tsrc/a%20b.rs:3-9\trun\n  4: fn run() {"
        );
    }

    #[test]
    fn single_repo_coverage_lists_nonzero_counters_and_statuses() {
        let coverage = json!({
            "total_files": 12, "indexed_files": 11, "excluded_files": 1,
            "parse_failures": 0, "walk_failures": 0, "truncated_files": 0,
            "semantic_status": "ready", "rerank_status": "unavailable"
        });
        assert_eq!(
            single_repo_coverage(&coverage),
            "indexed 11/12; excluded 1; rerank unavailable"
        );
    }

    #[test]
    fn page_text_ends_with_coverage_and_footer() {
        let entries = [hit("a"), hit("b")];
        let next = next_cursor("00000000000000000000000000000000", 0, 2, 5);
        let text = page_text(
            entries.iter().map(|entry| (None, entry)),
            named(true),
            "indexed 1/1",
            next.as_deref(),
            true,
        );
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[2], "coverage: indexed 1/1");
        assert_eq!(lines[3], "next: 00000000000000000000000000000000@2");
        assert_eq!(lines[4], "truncated: true");
    }
}
