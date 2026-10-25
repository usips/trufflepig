//! Cache-free search: walk, order, prefilter and read current files into an
//! unpublished store, then run the normal lanes over what was read. `kind:file`
//! text answers path matches first and reads bodies only for a short page.
use super::{
    CurrentSearchOptions,
    coverage::{self, CurrentCoverage, FollowUp, LIMIT_BODIES_SKIPPED, LIMIT_DEADLINE},
    current_walk::{self, MAX_WALK_ENTRIES},
    handles,
    literal_prefilter::LiteralPrefilter,
    read_cutoff::ReadCutoff,
    source_reads::{self, MAX_CURRENT_FILE_BYTES, MAX_CURRENT_FILES},
};
use crate::{
    results::{Hit, MAX_HITS, ResultSet},
    search::{self, InvocationDirectory, Query, telemetry::RetrievalTrace},
    source,
    store::{self, Store},
};
use anyhow::{Result, bail, ensure};
use serde_json::json;
use std::{collections::HashMap, path::Path};
use tp_core::{coded_error::ErrorCode, query_deadline::QueryDeadline};

const MAX_SOURCE_BYTES_FOR_FILENAME_HITS: usize = 32 * 1024 * 1024;

pub(super) fn search_root(
    root: &Path,
    cache_exclusion: &Path,
    query: &Query,
    origin: &InvocationDirectory,
    deadline: &QueryDeadline,
    options: CurrentSearchOptions,
    mut cutoff: ReadCutoff,
) -> Result<ResultSet> {
    check_entry(deadline)?;
    let store = Store::unpublished(root, cache_exclusion)?.with_query_deadline(deadline.clone());
    validate_cache_exclusion(&store.root, cache_exclusion)?;
    let mut current = CurrentCoverage::default();
    let needs_path_catalog = !query.path.is_empty();
    let file_query = !query.regex && query.kind == "file";
    let filename_only = file_query && query.text.is_empty();
    let live_regex_only = query.regex && !needs_path_catalog;
    let mut prefilter = LiteralPrefilter::for_search(query);
    let mut effective_query = Query {
        text: query.text.clone(),
        path: query.path.clone(),
        language: query.language.clone(),
        kind: query.kind.clone(),
        exact: query.exact,
        regex: query.regex,
    };
    // A linked worktree stores its Git pointer as a file rather than a directory.
    effective_query.path.exclude(".git");
    let mut paths = Vec::new();
    let mut path_first = None;

    if !live_regex_only {
        let path_limit = if file_query || needs_path_catalog || prefilter.is_some() {
            MAX_WALK_ENTRIES
        } else {
            MAX_CURRENT_FILES
        };
        let start = store.root.clone();
        paths = current_walk::current_paths(
            &store,
            &start,
            cache_exclusion,
            path_limit,
            &mut cutoff,
            &mut current,
        )?;
        if needs_path_catalog && (!current.complete || current.path_catalog_truncated) {
            // `file:` binding needs the whole catalog; a partial one proves nothing.
            current.complete = false;
            current.unsearched_lower_bound |= current.path_catalog_truncated;
            current.unsearched_files = current.unsearched_files.max(1);
            return Ok(empty_set(current, query, options));
        }
        store::current_source::register_paths(&store, paths.iter().map(String::as_str))?;
        if !query.regex && !filename_only {
            if needs_path_catalog {
                let selected = query.path.bind(&store.conn)?;
                paths.retain(|path| selected.matches(path));
            }
            current_walk::order_for_reading(&mut paths, origin);
            if file_query {
                let found = run_lanes(&store, &effective_query, cache_exclusion, deadline)?;
                if found.hits.len() >= options.page_limit.max(1) {
                    current.body_lane = Some("skipped");
                    current.stop(LIMIT_BODIES_SKIPPED, paths.len(), None);
                } else {
                    source_reads::read_current_sources(
                        &store,
                        &paths,
                        prefilter.as_mut(),
                        &mut cutoff,
                        &mut current,
                    )?;
                    current.body_lane = Some(if current.limit.is_some() {
                        "partial"
                    } else {
                        "complete"
                    });
                }
                path_first = Some(found);
            } else {
                source_reads::read_current_sources(
                    &store,
                    &paths,
                    prefilter.as_mut(),
                    &mut cutoff,
                    &mut current,
                )?;
            }
        }
    }

    ensure!(
        !deadline.cancelled(),
        coded!(TimedOut, "stateless current-source query was cancelled")
    );
    let mut set = match path_first {
        Some(found) if current.body_lane == Some("skipped") => found,
        Some(found) => {
            let mut set = run_lanes(&store, &effective_query, cache_exclusion, deadline)?;
            put_path_hits_first(&mut set, found.hits);
            set
        }
        None => run_lanes(&store, &effective_query, cache_exclusion, deadline)?,
    };
    if query.exact {
        search::rank_definitions(&mut set.hits, origin);
    }
    if query.regex {
        current = live_coverage(&set, query);
        if needs_path_catalog {
            current.walked_files = paths.len();
        }
    }
    // A cache-free page has no continuation, so hits past a page (with headroom
    // for dropped ones) never need verified handles.
    let verified_hits = options.page_limit.saturating_mul(2).max(16);
    if set.hits.len() > verified_hits {
        set.hits.truncate(verified_hits);
        set.truncated = true;
    }
    if file_query {
        attach_filename_handles(&store.root, &mut set, deadline, &mut current)?;
    } else {
        handles::attach_current_handles(&store.root, &mut set, deadline, &mut current)?;
    }
    coverage::apply(
        &mut set,
        &current,
        FollowUp::Search(query),
        options.semantic_requested,
        options.rerank_requested,
    );
    Ok(set)
}

fn run_lanes(
    store: &Store,
    query: &Query,
    cache_exclusion: &Path,
    deadline: &QueryDeadline,
) -> Result<ResultSet> {
    let mut trace = RetrievalTrace::disabled();
    search::search_prepared(
        store,
        query,
        cache_exclusion,
        None,
        None,
        &mut trace,
        deadline,
    )
}

/// Path-lane hits lead in their own order (taking the body run's copy of the
/// same file when it has one); body-only matches follow.
fn put_path_hits_first(set: &mut ResultSet, path_hits: Vec<Hit>) {
    let mut body: HashMap<String, Hit> = HashMap::with_capacity(set.hits.len());
    let mut rest = Vec::with_capacity(set.hits.len());
    let named: std::collections::HashSet<&str> =
        path_hits.iter().map(|hit| hit.path.as_str()).collect();
    for hit in std::mem::take(&mut set.hits) {
        if named.contains(hit.path.as_str()) {
            body.entry(hit.path.clone()).or_insert(hit);
        } else {
            rest.push(hit);
        }
    }
    let mut hits: Vec<Hit> = Vec::with_capacity(path_hits.len() + rest.len());
    for hit in path_hits {
        hits.push(body.remove(&hit.path).unwrap_or(hit));
    }
    hits.extend(rest);
    set.truncated |= hits.len() > MAX_HITS;
    hits.truncate(MAX_HITS);
    set.hits = hits;
}

/// Verifies `kind:file` hits from current bytes in rank order; reading stops at
/// the deadline or byte cap and leaves the remainder unsearched.
fn attach_filename_handles(
    root: &Path,
    set: &mut ResultSet,
    deadline: &QueryDeadline,
    current: &mut CurrentCoverage,
) -> Result<()> {
    let mut hits = std::mem::take(&mut set.hits).into_iter();
    let mut retained = Vec::with_capacity(hits.len());
    while let Some(mut hit) = hits.next() {
        if deadline.expired() {
            current.stop(LIMIT_DEADLINE, 1 + hits.count(), None);
            break;
        }
        let relative = tp_core::path_codec::decode_path(&hit.path)?;
        let bytes = match source::read_contained(root, &relative, MAX_CURRENT_FILE_BYTES) {
            Ok(bytes) if !bytes.contains(&0) => bytes,
            Ok(_) => {
                current.excluded_files = current.excluded_files.saturating_add(1);
                current.complete = false;
                continue;
            }
            Err(error) if ErrorCode::SourceExcluded.is_code_of(&error) => {
                current.excluded_files = current.excluded_files.saturating_add(1);
                current.complete = false;
                continue;
            }
            Err(_) => {
                current.read_failures = current.read_failures.saturating_add(1);
                current.complete = false;
                continue;
            }
        };
        let previously_read = hit.revision.is_some();
        let next_bytes = if previously_read {
            current.bytes_read
        } else {
            current.bytes_read.saturating_add(bytes.len())
        };
        if next_bytes > MAX_SOURCE_BYTES_FOR_FILENAME_HITS {
            current.unsearched_files = current.unsearched_files.saturating_add(1 + hits.count());
            current.complete = false;
            break;
        }
        let revision = tp_core::identity::ContentRevision::of(&bytes).to_string();
        if let Some(expected) = &hit.revision
            && expected != &revision
        {
            bail!(coded!(
                StaleSource,
                "current file changed during stateless search; search again"
            ));
        }
        hit.revision = Some(revision);
        let end = if hit.kind == "file" && hit.start == 0 && hit.end == 0 {
            bytes.len()
        } else {
            hit.end
        };
        hit.handle = crate::source::acquisition::stateless_hit_handle(
            root, &relative, &bytes, hit.start, end,
        )?;
        if !previously_read {
            current.read_files = current.read_files.saturating_add(1);
        }
        current.bytes_read = next_bytes;
        retained.push(hit);
    }
    set.hits = retained;
    Ok(())
}

fn live_coverage(set: &ResultSet, query: &Query) -> CurrentCoverage {
    let value = &set.coverage;
    let checked = value["live_checked_files"].as_u64().unwrap_or(0) as usize;
    let excluded = value["live_excluded_files"].as_u64().unwrap_or(0) as usize;
    let read_failures = value["live_read_failures"].as_u64().unwrap_or(0) as usize;
    let walk_failures = value["live_walk_failures"].as_u64().unwrap_or(0) as usize;
    let mut current = CurrentCoverage {
        walked_files: checked
            .saturating_add(excluded)
            .saturating_add(read_failures),
        read_files: checked,
        excluded_files: excluded,
        read_failures,
        walk_failures,
        unsearched_files: 0,
        complete: read_failures == 0 && walk_failures == 0 && excluded == 0 && !set.truncated,
        ..CurrentCoverage::default()
    };
    if !query.kind.is_empty() {
        current.unsearched_unknown = true;
        current.complete = false;
    }
    current
}

/// A truncated empty page whose coverage explains why nothing was searched.
pub(super) fn empty_set(
    current: CurrentCoverage,
    query: &Query,
    options: CurrentSearchOptions,
) -> ResultSet {
    let mut set = ResultSet {
        generation: 0,
        coverage: json!({"source":"current_source"}),
        truncated: true,
        hits: Vec::new(),
    };
    coverage::apply(
        &mut set,
        &current,
        FollowUp::Search(query),
        options.semantic_requested,
        options.rerank_requested,
    );
    set
}

/// One root-relative path made only of normal components (`./` allowed).
pub(super) fn current_relative_path(path: &str) -> Result<std::path::PathBuf> {
    let path = path.strip_prefix("./").unwrap_or(path);
    let path = path.trim_end_matches('/');
    let relative = tp_core::path_codec::decode_path(path)?;
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        coded!(InvalidPath, "use one root-relative current path")
    );
    Ok(relative)
}

pub(super) fn validate_cache_exclusion(root: &Path, cache: &Path) -> Result<()> {
    ensure!(
        !root.starts_with(current_walk::normalized_path(cache)),
        coded!(
            InvalidCache,
            "selected cache cannot contain the repository root"
        )
    );
    Ok(())
}

/// Nothing has been read when a scan starts after its deadline, so that fails.
pub(super) fn check_entry(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        bail!(coded!(
            TimedOut,
            "stateless current-source query deadline expired before reading"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod filename_handle_tests {
    use super::*;
    use crate::search::telemetry::RetrievalTrace;
    use std::{fs, time::Duration};
    use tp_core::output::OutputBudget;

    #[test]
    fn unparsed_filename_hit_verifies_current_bytes_without_proving_body_coverage() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("repo");
        fs::create_dir(&root)?;
        let bytes = b"pub fn current_baseline() {}\n";
        fs::write(root.join("a05_baselines.rs"), bytes)?;
        let cache = directory.path().join("unused-cache");
        let store = Store::unpublished(&root, &cache)?;
        store::current_source::register_paths(&store, ["a05_baselines.rs"])?;
        let query = Query::parse("a05_baselines kind:file")?;
        let deadline = QueryDeadline::after(Duration::from_secs(5));
        let mut set = search::search_prepared(
            &store,
            &query,
            &cache,
            None,
            None,
            &mut RetrievalTrace::disabled(),
            &deadline,
        )?;
        assert_eq!(set.hits.len(), 1);
        assert!(set.hits[0].revision.is_none());
        let mut current = CurrentCoverage {
            complete: false,
            unsearched_files: 1,
            ..CurrentCoverage::default()
        };
        attach_filename_handles(&root, &mut set, &deadline, &mut current)?;
        assert_eq!(set.hits.len(), 1);
        let shown = source::show(&store, &set.hits[0].handle, &OutputBudget::new(1_000)?)?;
        let page: serde_json::Value = serde_json::from_str(&shown)?;
        assert_eq!(page["lines"][0]["text"], std::str::from_utf8(bytes)?);
        assert!(!current.complete);
        assert_eq!(current.unsearched_files, 1);
        assert_eq!(current.read_files, 1);
        assert_eq!(current.bytes_read, bytes.len());
        assert!(!cache.exists());

        fs::write(root.join("a05_baselines.rs"), b"pub fn changed() {}\n")?;
        let error = attach_filename_handles(&root, &mut set, &deadline, &mut current).unwrap_err();
        assert!(error.to_string().starts_with("stale_source:"));
        Ok(())
    }
}
