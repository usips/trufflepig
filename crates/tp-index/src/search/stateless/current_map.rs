//! Cache-free `map PATH`: one current file lists its members; a directory lists
//! one row per current file under it, read in path order within the file cap
//! and read window, so a large directory returns a partial outline.
use super::{
    coverage::{self, CurrentCoverage, FollowUp},
    current_walk, handles,
    read_cutoff::ReadCutoff,
    source_reads,
    stateless_scan::{check_entry, current_relative_path, validate_cache_exclusion},
};
use crate::{
    results::{Hit, MAX_HITS, ResultSet},
    search, source,
    store::{self, Store},
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::path::{Path, PathBuf};
use tp_core::{coded_error::CodeDetail, query_deadline::QueryDeadline};

/// Most files a directory outline walks and reads.
const MAX_DIRECTORY_MAP_FILES: usize = 256;

pub(super) fn map_current_path(
    root: &Path,
    cache_exclusion: &Path,
    path: &str,
    deadline: &QueryDeadline,
    cutoff: ReadCutoff,
) -> Result<ResultSet> {
    check_entry(deadline)?;
    let root = root
        .canonicalize()
        .context("canonicalize repository root")?;
    ensure!(root.is_dir(), "repository root is not a directory");
    validate_cache_exclusion(&root, cache_exclusion)?;
    let relative = current_relative_path(path)?;
    ensure!(
        !root
            .join(&relative)
            .starts_with(current_walk::normalized_path(cache_exclusion)),
        coded!(
            SourceExcluded,
            "current map path is inside the selected cache"
        )
    );
    ensure!(
        !relative.components().any(|part| match part {
            std::path::Component::Normal(name) => matches!(
                name.to_str(),
                Some(".git" | "target" | "node_modules" | ".trufflepig")
            ),
            _ => false,
        }),
        coded!(
            SourceExcluded,
            "current map path is inside an excluded directory"
        )
    );
    match source::read_contained(&root, &relative, source::MAX_READ_BYTES) {
        Ok(bytes) => map_current_file(&root, &relative, bytes),
        Err(error) if CodeDetail::NotRegularFile.is_anywhere_in(&error) => {
            map_current_directory(&root, cache_exclusion, &relative, deadline, cutoff)
        }
        Err(error) => Err(error),
    }
}

fn map_current_file(root: &Path, relative: &Path, bytes: Vec<u8>) -> Result<ResultSet> {
    let encoded_path = tp_core::path_codec::encode_path(relative);
    ensure!(
        !bytes.contains(&0),
        coded!(SourceExcluded, "current map path is binary")
    );
    let extraction =
        source::usable_extraction(&encoded_path, tp_extract::extract(&encoded_path, &bytes))?;
    let revision = tp_core::identity::ContentRevision::of(&bytes).to_string();
    let facts_indexed = source_reads::extraction_fact_count(&extraction);
    let mut definitions = extraction.definitions;
    search::outline_extracted(&mut definitions);
    let truncated = definitions.len() > MAX_HITS;
    definitions.truncate(MAX_HITS);
    let mut hits = Vec::with_capacity(definitions.len());
    for definition in definitions {
        let (start_line, end_line) = source::line_span(&bytes, definition.start, definition.end);
        let handle = crate::source::acquisition::stateless_hit_handle(
            root,
            relative,
            &bytes,
            definition.start,
            definition.end,
        )?;
        hits.push(Hit {
            handle,
            path: encoded_path.clone(),
            revision: Some(revision.clone()),
            start: definition.start,
            end: definition.end,
            start_line,
            end_line,
            name: definition.name,
            kind: definition.kind,
            container: definition.container,
            provenance: Some("stateless_current_source_map".into()),
            resolution: None,
            candidates: Vec::new(),
            target: None,
            repeats: None,
            snippet: None,
            differs: false,
        });
    }
    let mut current = CurrentCoverage {
        walked_files: 1,
        read_files: 1,
        bytes_read: bytes.len(),
        facts_indexed,
        ..CurrentCoverage::default()
    };
    if !matches!(
        extraction.status.as_str(),
        "ok" | "parsed" | "complete" | "lexical_only" | "recovered"
    ) {
        current.parse_failures = 1;
        current.complete = false;
    }
    current.complete &= !truncated;
    let mut set = ResultSet {
        generation: 0,
        coverage: json!({"source":"current_source_map"}),
        truncated,
        hits,
    };
    coverage::apply(&mut set, &current, FollowUp::Map, false, false);
    Ok(set)
}

fn map_current_directory(
    root: &Path,
    cache_exclusion: &Path,
    relative: &Path,
    deadline: &QueryDeadline,
    mut cutoff: ReadCutoff,
) -> Result<ResultSet> {
    let directory: PathBuf = root
        .join(relative)
        .canonicalize()
        .context("canonicalize current map directory")?;
    ensure!(
        directory.starts_with(root) && directory.is_dir(),
        coded!(
            InvalidPath,
            "current map directory escapes the repository root"
        )
    );
    let store = Store::unpublished(root, cache_exclusion)?.with_query_deadline(deadline.clone());
    let mut current = CurrentCoverage::default();
    let paths = current_walk::current_paths(
        &store,
        &directory,
        cache_exclusion,
        MAX_DIRECTORY_MAP_FILES,
        &mut cutoff,
        &mut current,
    )?;
    store::current_source::register_paths(&store, paths.iter().map(String::as_str))?;
    source_reads::read_current_sources(&store, &paths, None, &mut cutoff, &mut current)?;
    ensure!(
        !deadline.cancelled(),
        coded!(TimedOut, "stateless current-source map was cancelled")
    );
    let encoded = tp_core::path_codec::encode_path(relative);
    let mut set = search::map(&store, &format!("{encoded}/"))?;
    if let Some(object) = set.coverage.as_object_mut() {
        object.remove("no_indexed_path");
    }
    set.coverage["source"] = "current_source_map".into();
    handles::attach_current_handles(&store.root, &mut set, deadline, &mut current)?;
    coverage::apply(&mut set, &current, FollowUp::Map, false, false);
    Ok(set)
}
