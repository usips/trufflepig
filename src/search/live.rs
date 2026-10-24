use super::*;
use crate::coded_error::CodedContext;
use crate::daemon::deadline::QueryDeadline;
use anyhow::{Result, bail};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LiveRegexCheckpoint {
    BeforeCompile,
    WalkEntry,
    WalkBatch,
    BeforeRead,
    AfterRead,
    BeforeMatchScan,
    AfterMatchScan,
    BeforeHash,
    AfterHash,
    BeforeExtract,
    AfterExtract,
    RegexMatch,
    MatchBatch,
}

pub(super) fn live_regex(
    store: &Store,
    query: &Query,
    paths: &BoundPathFilter,
    cache: &std::path::Path,
    hits: &mut Vec<Hit>,
    coverage: &mut serde_json::Value,
    truncated: &mut bool,
    deadline: &QueryDeadline,
) -> Result<()> {
    live_regex_with_checkpoint(
        store,
        query,
        paths,
        cache,
        hits,
        coverage,
        truncated,
        &mut |_, checkpoint| check_live_deadline(deadline, checkpoint),
    )
}

fn check_live_deadline(deadline: &QueryDeadline, checkpoint: LiveRegexCheckpoint) -> Result<()> {
    let expired = match checkpoint {
        LiveRegexCheckpoint::BeforeCompile
        | LiveRegexCheckpoint::WalkBatch
        | LiveRegexCheckpoint::BeforeRead
        | LiveRegexCheckpoint::MatchBatch => deadline.expired(),
        _ => deadline.time_expired(),
    };
    if expired {
        bail!(coded!(
            TimedOut,
            "live regex deadline expired or client disconnected"
        ));
    }
    Ok(())
}

fn live_regex_with_checkpoint(
    store: &Store,
    query: &Query,
    paths: &BoundPathFilter,
    cache: &std::path::Path,
    hits: &mut Vec<Hit>,
    coverage: &mut serde_json::Value,
    truncated: &mut bool,
    checkpoint: &mut impl FnMut(&Path, LiveRegexCheckpoint) -> Result<()>,
) -> Result<()> {
    let result = live_regex_scan(
        store, query, paths, cache, hits, coverage, truncated, checkpoint,
    );
    match result {
        Err(error) if crate::coded_error::ErrorCode::TimedOut.is_code_of(&error) => Err(error
            .with_coded_context(|| {
                coded!(
                    TimedOut,
                    "live regex scan incomplete; discarded {} partial hits",
                    hits.len()
                )
            })),
        result => result,
    }
}

fn live_regex_scan(
    store: &Store,
    query: &Query,
    paths: &BoundPathFilter,
    cache: &std::path::Path,
    hits: &mut Vec<Hit>,
    coverage: &mut serde_json::Value,
    truncated: &mut bool,
    checkpoint: &mut impl FnMut(&Path, LiveRegexCheckpoint) -> Result<()>,
) -> Result<()> {
    // Line anchors match at every line, as in grep.
    checkpoint(&store.root, LiveRegexCheckpoint::BeforeCompile)?;
    let regex = regex::bytes::RegexBuilder::new(&query.text)
        .multi_line(true)
        .size_limit(8 * 1024 * 1024)
        .build()?;
    let mut failures = 0usize;
    let mut excluded = 0usize;
    let mut walk_failures = 0usize;
    let mut walk_entries = 0usize;
    let mut checked = 0usize;
    let mut match_iterations = 0usize;
    // Neither the index cache nor a worktree view's own cache is source.
    let caches = [cache, store.results_cache()]
        .map(|cache| cache.canonicalize().unwrap_or_else(|_| cache.to_owned()));
    let mut walker = ignore::WalkBuilder::new(&store.root);
    walker
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b));
    walker.filter_entry(move |entry| {
        !caches.iter().any(|cache| entry.path().starts_with(cache))
            && (!entry.file_type().is_some_and(|kind| kind.is_dir())
                || !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "target" | "node_modules" | ".trufflepig")
                ))
    });
    for entry in walker.build() {
        let checkpoint_path = entry
            .as_ref()
            .map(|entry| entry.path())
            .unwrap_or_else(|_| store.root.as_path());
        checkpoint(checkpoint_path, LiveRegexCheckpoint::WalkEntry)?;
        // Filter-rejected entries never reach a read, so poll the peer on a walk cadence too.
        walk_entries += 1;
        if walk_entries % 1024 == 0 {
            checkpoint(checkpoint_path, LiveRegexCheckpoint::WalkBatch)?;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                walk_failures += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry.path().strip_prefix(&store.root)?;
        let path = crate::store::encode_path(relative);
        let language = crate::extract::language(&path);
        if !paths.matches(&path) || (!query.language.is_empty() && query.language != language) {
            continue;
        }
        // Oversized and binary files are excluded by contract, not unsearched.
        checkpoint(entry.path(), LiveRegexCheckpoint::BeforeRead)?;
        let read = source::read_contained(&store.root, relative, source::MAX_READ_BYTES);
        checkpoint(entry.path(), LiveRegexCheckpoint::AfterRead)?;
        let source = match read {
            Ok(bytes) => bytes,
            Err(error) if crate::coded_error::ErrorCode::SourceExcluded.is_code_of(&error) => {
                excluded += 1;
                continue;
            }
            Err(_) => {
                failures += 1;
                continue;
            }
        };
        if source.contains(&0) {
            excluded += 1;
            continue;
        }
        checked += 1;
        checkpoint(entry.path(), LiveRegexCheckpoint::BeforeMatchScan)?;
        let mut matches = regex.find_iter(&source);
        let first_match = matches.next();
        checkpoint(entry.path(), LiveRegexCheckpoint::AfterMatchScan)?;
        let Some(first_match) = first_match else {
            continue;
        };
        checkpoint(entry.path(), LiveRegexCheckpoint::BeforeHash)?;
        let revision = blake3::hash(&source).to_hex().to_string();
        checkpoint(entry.path(), LiveRegexCheckpoint::AfterHash)?;
        let extraction = if query.kind.is_empty() {
            None
        } else {
            checkpoint(entry.path(), LiveRegexCheckpoint::BeforeExtract)?;
            let extraction = Some(crate::extract::extract(&path, &source));
            checkpoint(entry.path(), LiveRegexCheckpoint::AfterExtract)?;
            extraction
        };
        // One hit per matching line: later matches on the same line add nothing.
        let mut previous_line = None;
        for matched in std::iter::once(first_match).chain(matches) {
            checkpoint(entry.path(), LiveRegexCheckpoint::RegexMatch)?;
            match_iterations += 1;
            if match_iterations % 64 == 0 {
                checkpoint(entry.path(), LiveRegexCheckpoint::MatchBatch)?;
            }
            let (start_line, end_line) = source::line_span(&source, matched.start(), matched.end());
            if previous_line == Some(start_line) {
                continue;
            }
            let definition = extraction.as_ref().and_then(|e| {
                e.definitions
                    .iter()
                    .filter(|d| {
                        d.start <= matched.start()
                            && d.end >= matched.end()
                            && (query.kind.is_empty() || query.kind == d.kind)
                    })
                    .min_by_key(|d| d.end - d.start)
            });
            if !query.kind.is_empty() && definition.is_none() {
                continue;
            }
            if hits.len() >= MAX_HITS {
                *truncated = true;
                break;
            }
            previous_line = Some(start_line);
            hits.push(Hit {
                handle: String::new(),
                path: path.clone(),
                revision: Some(revision.clone()),
                start: matched.start(),
                end: matched.end(),
                start_line,
                end_line,
                name: String::from_utf8_lossy(matched.as_bytes())
                    .chars()
                    .take(96)
                    .collect(),
                kind: definition
                    .map(|d| d.kind.clone())
                    .unwrap_or_else(|| "region".into()),
                container: definition.map(|d| d.name.clone()),
                provenance: Some("live_regex".into()),
                resolution: None,
                candidates: Vec::new(),
                target: None,
                repeats: None,
                snippet: super::snippets::preview(
                    &source,
                    matched.start(),
                    matched.end(),
                    start_line,
                    &[],
                    "",
                ),
                differs: false,
            });
        }
        if *truncated {
            break;
        }
    }
    coverage["live_checked_files"] = checked.into();
    coverage["live_read_failures"] = failures.into();
    coverage["live_excluded_files"] = excluded.into();
    coverage["live_walk_failures"] = walk_failures.into();
    Ok(())
}

#[cfg(test)]
mod tests;
