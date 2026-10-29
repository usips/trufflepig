//! Complete current-source symbol acquisition without persistent result or index caches.
use super::{
    coverage::{self, CurrentCoverage, FollowUp},
    current_walk::{self, MAX_WALK_ENTRIES},
    literal_prefilter::LiteralPrefilter,
    read_cutoff::ReadCutoff,
    source_reads::{self, MAX_CURRENT_FILES},
    stateless_scan::{check_entry, validate_cache_exclusion},
};
use crate::{
    search::{self, InvocationDirectory, Query},
    store::{self, Store},
};
use anyhow::{Result, bail, ensure};
use std::path::Path;
use tp_core::{coded_error::ErrorCode, query_deadline::QueryDeadline};

pub(super) fn show_symbol_root(
    root: &Path,
    cache_exclusion: &Path,
    target: &str,
    origin: &InvocationDirectory,
    deadline: &QueryDeadline,
) -> Result<super::StatelessSymbolRead> {
    ensure!(
        target.starts_with("sym:"),
        coded!(Usage, "show sym:NAME [file:P] [lang:L] [kind:K]")
    );
    check_entry(deadline)?;
    let query = Query::parse(target)?;
    let store = Store::unpublished(root, cache_exclusion)?.with_query_deadline(deadline.clone());
    validate_cache_exclusion(&store.root, cache_exclusion)?;

    let mut current = CurrentCoverage::default();
    let mut cutoff = ReadCutoff::for_deadline(deadline);
    let needs_path_catalog = !query.path.is_empty();
    let mut prefilter = LiteralPrefilter::for_search(&query);
    let path_limit = if needs_path_catalog || prefilter.is_some() {
        MAX_WALK_ENTRIES
    } else {
        MAX_CURRENT_FILES
    };
    let start = store.root.clone();
    let mut all_paths = current_walk::current_paths(
        &store,
        &start,
        cache_exclusion,
        path_limit,
        &mut cutoff,
        &mut current,
    )?;
    store::current_source::register_paths(&store, all_paths.iter().map(String::as_str))?;
    if needs_path_catalog && (!current.complete || current.path_catalog_truncated) {
        current.complete = false;
        current.unsearched_lower_bound |= current.path_catalog_truncated;
        current.unsearched_files = current.unsearched_files.max(1);
    } else {
        let supplement_paths = all_paths.clone();
        if needs_path_catalog {
            let selected = query.path.bind(&store.conn)?;
            all_paths.retain(|path| selected.matches(path));
        }
        current_walk::order_for_reading(&mut all_paths, origin);
        let mut counted_prefiltered = std::collections::HashSet::with_capacity(all_paths.len());
        source_reads::read_current_sources_tracked(
            &store,
            &all_paths,
            prefilter.as_mut(),
            &mut cutoff,
            &mut current,
            Some(&mut counted_prefiltered),
        )?;
        if current.complete && current.limit.is_none() {
            let mut candidates = search::definitions(&store, &query, origin)?;
            if candidates.hits.is_empty() && needs_path_catalog {
                let Some(mut namesake_prefilter) = LiteralPrefilter::for_search(&query) else {
                    bail!(coded!(Usage, "show sym:NAME"));
                };
                let unread = unread_paths(&store, &supplement_paths)?;
                source_reads::read_current_sources_tracked(
                    &store,
                    &unread,
                    Some(&mut namesake_prefilter),
                    &mut cutoff,
                    &mut current,
                    Some(&mut counted_prefiltered),
                )?;
                candidates = search::definitions(&store, &query, origin)?;
            }
            if let Some(import) = candidates.hits.first().filter(|hit| hit.kind == "import") {
                for step in 0..=crate::search::IMPORT_HOPS {
                    let Some((trail, Some(target))) =
                        crate::search::trace_import_with_target(&store, import, &query, origin)?
                    else {
                        break;
                    };
                    if trail.declaration.is_some() || step == crate::search::IMPORT_HOPS {
                        break;
                    }
                    let Some(mut target_prefilter) = LiteralPrefilter::for_name(&target) else {
                        break;
                    };
                    let unread = unread_paths(&store, &supplement_paths)?;
                    if unread.is_empty() {
                        break;
                    }
                    let before = current.read_files;
                    source_reads::read_current_sources_tracked(
                        &store,
                        &unread,
                        Some(&mut target_prefilter),
                        &mut cutoff,
                        &mut current,
                        Some(&mut counted_prefiltered),
                    )?;
                    if current.read_files == before || current.limit.is_some() {
                        break;
                    }
                }
            }
        }
    }
    ensure!(
        !deadline.cancelled(),
        coded!(TimedOut, "cache-free symbol lookup was cancelled")
    );

    let mut found = search::definitions(&store, &query, origin)?;
    coverage::apply(&mut found, &current, FollowUp::Search(&query), false, false);
    ensure!(
        found.coverage["complete"] == true,
        coded!(
            SourceUnavailable,
            "cache-free symbol lookup could not rank a complete current-source snapshot; coverage={}",
            found.coverage
        )
    );
    let coverage = found.coverage.clone();
    let source =
        crate::source::acquisition::read_symbol_without_cache(&store, &query, found, origin)?;
    Ok(super::StatelessSymbolRead { source, coverage })
}

fn unread_paths(store: &Store, paths: &[String]) -> Result<Vec<String>> {
    let mut statement = store
        .conn
        .prepare("SELECT status='unread' AND revision IS NULL FROM files WHERE path=?1")?;
    let mut unread = Vec::with_capacity(paths.len());
    for path in paths {
        if statement.query_row([path], |row| row.get::<_, bool>(0))? {
            unread.push(path.clone());
        }
    }
    Ok(unread)
}
