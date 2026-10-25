//! `lines` rendering of result pages: one tab-separated line per entry, a
//! one-line coverage summary, then the shared footer. The budget measures this
//! text, so a lines page carries more hits than the JSON page for the same limit.

use super::{HitDetail, ResultEntry, concise_name};
use serde_json::Value;
use std::fmt::Write;
use tp_core::output::lines_format::{
    COVERAGE_KEY, NextCommand, PAGE_BUDGET_HINT, PAGE_SIZE_HINT, footer,
};

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
            if let Some(repeats) = hit.repeats {
                let _ = write!(line, "\t×{repeats}");
            }
            if hit.differs {
                line.push_str("\tdiffers");
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

/// Whether a lane reason means the lane was never configured rather than failed;
/// coverage summaries omit such lanes.
pub fn unconfigured(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| reason.contains("model_dir") || reason.contains("not configured"))
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
    let mut partial = Vec::with_capacity(3);
    let unsearched = count("current_source_unsearched");
    if unsearched > 0 {
        partial.push(format!("{unsearched} unsearched: current_source_recheck"));
    }
    for (key, label) in [
        ("live_read_failures", "read_error"),
        ("live_walk_failures", "walk_error"),
    ] {
        let value = count(key);
        if value > 0 {
            partial.push(format!("{label} {value}"));
        }
    }
    if !partial.is_empty() {
        parts.push(format!("partial ({})", partial.join("; ")));
    }
    for (label, key, reason) in [
        ("semantic", "semantic_status", "semantic_reason"),
        ("rerank", "rerank_status", "rerank_reason"),
    ] {
        if let Some(status) = coverage[key].as_str()
            && status != "ready"
            && !unconfigured(coverage[reason].as_str())
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
    if let Some(progress) = coverage
        .get("index_progress")
        .and_then(index_progress_summary)
    {
        parts.push(progress);
    }
    if coverage["state"] == "parent_fallback" {
        if let Some(served_from) = coverage["served_from"].as_str() {
            parts.push(format!("worktree index warming; served from {served_from}"));
        }
        if let Some(differing_hits) = coverage["differing_hits"].as_u64()
            && differing_hits > 0
        {
            parts.push(format!("{differing_hits} differing hits"));
        }
    }
    if let Some(diagnosis) = coverage["filter_diagnosis"].as_str() {
        parts.push(diagnosis.to_owned());
    }
    parts.extend(crate::search::reference_summary(coverage));
    parts.join("; ")
}

/// Summarizes retained index preparation without presenting it as published
/// coverage. Progress is present only while a preparation remains incomplete.
pub fn index_progress_summary(progress: &Value) -> Option<String> {
    let phase = progress.get("phase")?.as_str()?;
    let completed = progress.get("completed_files")?.as_u64()?;
    let reused = progress.get("reused_files")?.as_u64()?;
    let maintenance_required = progress.get("maintenance_required")?.as_bool()?;
    let detail = format!("retained preparation: {completed} files; reused {reused} extracts");
    if maintenance_required || phase == "maintenance_required" {
        return Some(format!(
            "index maintenance required ({detail}); run `trufflepig index`"
        ));
    }
    match phase {
        "scanning" => Some(format!("index scanning ({detail})")),
        "finalizing" => Some(format!("index finalizing ({detail})")),
        _ => None,
    }
}

/// Renders a page: the entries, the coverage line, then the footer.
pub fn page_text<'a>(
    entries: impl ExactSizeIterator<Item = (Option<&'a str>, &'a ResultEntry)>,
    detail: HitDetail,
    coverage: &str,
    next: Option<NextCommand<'_>>,
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
pub fn next_cursor(id: &str, offset: usize, count: usize, total: usize) -> Option<String> {
    (offset + count < total).then(|| format!("{id}@{}", offset + count))
}

/// `more CURSOR` for a page of `count` hits under a `limit`-hit cap. The first
/// page names the option that widens it: `-n` when the cap cut it, else `-b`.
pub fn more_command(cursor: &str, offset: usize, count: usize, limit: usize) -> NextCommand<'_> {
    NextCommand {
        verb: "more",
        cursor,
        hint: (offset == 0).then_some(if count >= limit {
            PAGE_SIZE_HINT
        } else {
            PAGE_BUDGET_HINT
        }),
    }
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
            repeats: None,
            snippet: Some(crate::results::Snippet {
                line: 4,
                text: "fn run() {".into(),
            }),
            differs: false,
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
    fn single_repo_coverage_renders_present_index_progress_only() {
        let scanning = json!({
            "total_files": 12,
            "indexed_files": 11,
            "index_progress": {
                "phase": "scanning",
                "completed_files": 17,
                "reused_files": 5,
                "maintenance_required": false
            }
        });
        let summary = single_repo_coverage(&scanning);
        assert_eq!(
            summary,
            "indexed 11/12; index scanning (retained preparation: 17 files; reused 5 extracts)"
        );
        assert!(summary.len() < 120, "progress summary should stay concise");

        let finalizing = json!({
            "total_files": 12,
            "indexed_files": 11,
            "index_progress": {
                "phase": "finalizing",
                "completed_files": 17,
                "reused_files": 5,
                "maintenance_required": false
            }
        });
        assert_eq!(
            single_repo_coverage(&finalizing),
            "indexed 11/12; index finalizing (retained preparation: 17 files; reused 5 extracts)"
        );

        let maintenance = json!({
            "total_files": 12,
            "indexed_files": 11,
            "index_progress": {
                "phase": "maintenance_required",
                "completed_files": 17,
                "reused_files": 5,
                "maintenance_required": true
            }
        });
        assert_eq!(
            single_repo_coverage(&maintenance),
            "indexed 11/12; index maintenance required (retained preparation: 17 files; reused 5 extracts); run `trufflepig index`"
        );

        let complete = json!({ "total_files": 12, "indexed_files": 12 });
        assert_eq!(single_repo_coverage(&complete), "indexed 12/12");
    }

    #[test]
    fn live_read_and_walk_failures_mark_even_fully_indexed_coverage_partial() {
        let coverage = json!({
            "total_files": 12,
            "indexed_files": 12,
            "live_read_failures": 2,
            "live_walk_failures": 3,
        });
        assert_eq!(
            single_repo_coverage(&coverage),
            "indexed 12/12; partial (read_error 2; walk_error 3)"
        );
    }

    #[test]
    fn page_text_ends_with_coverage_and_footer() {
        let entries = [hit("a"), hit("b")];
        let next = next_cursor("00000000000000000000000000000000", 0, 2, 5).unwrap();
        let text = page_text(
            entries.iter().map(|entry| (None, entry)),
            named(true),
            "indexed 1/1",
            Some(more_command(&next, 0, 2, 2)),
            true,
        );
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[2], "coverage: indexed 1/1");
        assert_eq!(lines[3], "next: more 00000000000000000000000000000000@2");
        assert_eq!(lines[4], "hint: -n N raises the page size");
        assert_eq!(lines[5], "truncated: true");
        // Later pages and budget-cut pages.
        assert_eq!(more_command(&next, 2, 2, 20).hint, None);
        assert_eq!(more_command(&next, 0, 2, 20).hint, Some(PAGE_BUDGET_HINT));
    }
}
