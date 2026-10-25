//! Cache-free `refs NAME`: only files whose path or bytes contain NAME are
//! extracted, global name candidates are resolved among them, and the normal
//! reference query runs over that in-memory snapshot. Every definition named
//! NAME lies in such a file, so complete coverage keeps candidate evidence exact.
use super::{
    coverage::{self, CurrentCoverage, FollowUp},
    current_walk::{self, MAX_WALK_ENTRIES},
    handles,
    literal_prefilter::LiteralPrefilter,
    read_cutoff::ReadCutoff,
    source_reads,
    stateless_scan::{check_entry, validate_cache_exclusion},
};
use crate::{
    results::ResultSet,
    search::{self, InvocationDirectory},
    store::{self, Store},
};
use anyhow::{Result, ensure};
use serde_json::json;
use std::path::Path;
use tp_core::query_deadline::QueryDeadline;

pub(super) fn refs_root(
    root: &Path,
    cache_exclusion: &Path,
    text: &str,
    origin: &InvocationDirectory,
    deadline: &QueryDeadline,
    mut cutoff: ReadCutoff,
) -> Result<ResultSet> {
    check_entry(deadline)?;
    let query = search::reference_query(text)?;
    let mut prefilter = LiteralPrefilter::for_name(&query.text)
        .ok_or_else(|| anyhow::anyhow!(coded!(Usage, "refs NAME")))?;
    let mut store =
        Store::unpublished(root, cache_exclusion)?.with_query_deadline(deadline.clone());
    validate_cache_exclusion(&store.root, cache_exclusion)?;
    let mut current = CurrentCoverage::default();
    let start = store.root.clone();
    let mut paths = current_walk::current_paths(
        &store,
        &start,
        cache_exclusion,
        MAX_WALK_ENTRIES,
        &mut cutoff,
        &mut current,
    )?;
    let refused = !query.path.is_empty() && !(current.complete && !current.path_catalog_truncated);
    if !refused {
        store::current_source::register_paths(&store, paths.iter().map(String::as_str))?;
        if !query.path.is_empty() {
            let selected = query.path.bind(&store.conn)?;
            paths.retain(|path| selected.matches(path));
        }
        current_walk::order_for_reading(&mut paths, origin);
        source_reads::read_current_sources(
            &store,
            &paths,
            Some(&mut prefilter),
            &mut cutoff,
            &mut current,
        )?;
    } else {
        // `file:` binding needs the whole catalog; a partial one proves nothing.
        current.complete = false;
        current.unsearched_lower_bound |= current.path_catalog_truncated;
        current.unsearched_files = current.unsearched_files.max(1);
    }
    ensure!(
        !deadline.cancelled(),
        coded!(TimedOut, "stateless current-source refs were cancelled")
    );
    store::current_source::resolve_name_candidates(&mut store)?;
    let mut set = if refused {
        ResultSet {
            generation: 0,
            coverage: json!({}),
            truncated: true,
            hits: Vec::new(),
        }
    } else {
        search::references(&store, &query)?
    };
    set.coverage["source"] = "current_source_refs".into();
    set.coverage["reference_resolution"] = "name_candidates_in_current_files".into();
    handles::attach_current_handles(&store.root, &mut set, deadline, &mut current)?;
    coverage::apply(&mut set, &current, FollowUp::Refs(&query), false, false);
    Ok(set)
}
