use super::{coverage, handles};
use crate::{
    coded_error::{CodeDetail, ErrorCode, error_detail},
    daemon::deadline::QueryDeadline,
    results::{MAX_HITS, ResultSet},
    search::{self, InvocationDirectory, Query, telemetry::RetrievalTrace},
    source::{self, MAX_READ_BYTES},
    store::{self, Store},
};
use anyhow::{Context, Result, bail, ensure};
use ignore::WalkBuilder;
use serde_json::json;
use std::path::Path;

const MAX_CURRENT_FILES: usize = 4_096;
const MAX_WALK_ENTRIES: usize = 65_536;
const MAX_FILENAME_PATHS: usize = MAX_WALK_ENTRIES;
pub(super) const MAX_CURRENT_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CURRENT_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_CURRENT_SOURCE_FACTS: usize = 250_000;

pub(super) fn search_root(
    root: &Path,
    cache_exclusion: &Path,
    query: &Query,
    origin: &InvocationDirectory,
    deadline: &QueryDeadline,
    semantic_requested: bool,
    rerank_requested: bool,
) -> Result<ResultSet> {
    check_deadline(deadline)?;
    let store = Store::unpublished(root, cache_exclusion)?.with_query_deadline(deadline.clone());
    validate_cache_exclusion(&store.root, cache_exclusion)?;
    let mut current = coverage::CurrentCoverage::default();
    let needs_path_catalog = !query.path.is_empty();
    let file_query = !query.regex && query.kind == "file";
    let filename_only = file_query && query.text.is_empty();
    let live_regex_only = query.regex && !needs_path_catalog;
    let mut paths = Vec::new();

    if !live_regex_only {
        let path_limit = if file_query || needs_path_catalog {
            MAX_FILENAME_PATHS
        } else {
            MAX_CURRENT_FILES
        };
        paths = current_paths(&store, cache_exclusion, path_limit, deadline, &mut current)?;
        if needs_path_catalog && (!current.complete || current.path_catalog_truncated) {
            current.complete = false;
            current.unsearched_lower_bound |= current.path_catalog_truncated;
            current.unsearched_files = current.unsearched_files.max(1);
            return Ok(empty_set(
                current,
                query,
                semantic_requested,
                rerank_requested,
            ));
        }
        store::current_source::register_paths(&store, paths.iter().map(String::as_str))?;
        if !query.regex && !filename_only {
            if needs_path_catalog {
                let selected = query.path.bind(&store.conn)?;
                paths.retain(|path| selected.matches(path));
            }
            read_current_sources(&store, &paths, deadline, &mut current)?;
        }
    }

    check_deadline(deadline)?;
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
    let mut trace = RetrievalTrace::disabled();
    let mut set = search::search_prepared(
        &store,
        &effective_query,
        cache_exclusion,
        None,
        None,
        &mut trace,
        deadline,
    )?;
    if query.exact {
        search::rank_definitions(&mut set.hits, origin);
    }

    if query.regex {
        current = live_coverage(&set, query);
        if needs_path_catalog {
            current.walked_files = paths.len();
        }
    }
    if file_query {
        attach_filename_handles(&store.root, &mut set, deadline, &mut current)?;
    } else {
        let missing_handles = handles::attach_current_handles(&store.root, &mut set, deadline)?;
        if missing_handles > 0 {
            current.unsearched_files = current.unsearched_files.saturating_add(missing_handles);
            current.complete = false;
        }
    }
    coverage::apply(
        &mut set,
        &current,
        query,
        semantic_requested,
        rerank_requested,
    );
    Ok(set)
}

pub(super) fn map_current_file(
    root: &Path,
    cache_exclusion: &Path,
    path: &str,
    deadline: &QueryDeadline,
) -> Result<ResultSet> {
    check_deadline(deadline)?;
    let root = root
        .canonicalize()
        .context("canonicalize repository root")?;
    ensure!(root.is_dir(), "repository root is not a directory");
    validate_cache_exclusion(&root, cache_exclusion)?;
    let relative = current_relative_path(path)?;
    ensure!(
        !root
            .join(&relative)
            .starts_with(normalized_path(cache_exclusion)),
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
    let encoded_path = store::encode_path(&relative);
    let bytes = match source::read_contained(&root, &relative, MAX_READ_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error_detail(&error) == Some(CodeDetail::NotRegularFile) => {
            bail!(coded!(
                StatelessMapRequiresFile,
                "map requires one current file path"
            ))
        }
        Err(error) => return Err(error),
    };
    check_deadline(deadline)?;
    ensure!(
        !bytes.contains(&0),
        coded!(SourceExcluded, "current map path is binary")
    );
    let extraction = source::usable_extraction(
        &encoded_path,
        crate::extract::extract(&encoded_path, &bytes),
    )?;
    check_deadline(deadline)?;
    let revision = crate::identity::ContentRevision::of(&bytes).to_string();
    let facts_indexed = extraction_fact_count(&extraction);
    let mut definitions = extraction.definitions;
    search::outline_extracted(&mut definitions);
    let truncated = definitions.len() > MAX_HITS;
    definitions.truncate(MAX_HITS);
    let mut hits = Vec::with_capacity(definitions.len());
    for definition in definitions {
        check_deadline(deadline)?;
        let (start_line, end_line) = source::line_span(&bytes, definition.start, definition.end);
        let handle = crate::source::acquisition::stateless_hit_handle(
            &root,
            &relative,
            &bytes,
            definition.start,
            definition.end,
        )?;
        hits.push(crate::results::Hit {
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
    check_deadline(deadline)?;
    let mut current = coverage::CurrentCoverage {
        walked_files: 1,
        read_files: 1,
        bytes_read: bytes.len(),
        facts_indexed,
        ..coverage::CurrentCoverage::default()
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
    coverage::apply(&mut set, &current, &Query::default(), false, false);
    Ok(set)
}

fn current_paths(
    store: &Store,
    cache_exclusion: &Path,
    max_paths: usize,
    deadline: &QueryDeadline,
    current: &mut coverage::CurrentCoverage,
) -> Result<Vec<String>> {
    let mut caches = vec![
        normalized_path(cache_exclusion),
        normalized_path(store.results_cache()),
    ];
    caches.sort();
    caches.dedup();
    let mut walker = WalkBuilder::new(&store.root);
    walker
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|left, right| left.cmp(right));
    walker.filter_entry(move |entry| {
        entry.file_name() != ".git"
            && !caches.iter().any(|cache| entry.path().starts_with(cache))
            && (!entry.file_type().is_some_and(|kind| kind.is_dir())
                || !matches!(
                    entry.file_name().to_str(),
                    Some("target" | "node_modules" | ".trufflepig")
                ))
    });
    let mut paths = Vec::with_capacity(max_paths.min(MAX_WALK_ENTRIES));
    let mut entries = 0usize;
    for entry in walker.build() {
        check_deadline(deadline)?;
        entries += 1;
        if entries > MAX_WALK_ENTRIES {
            current.path_catalog_truncated = true;
            current.unsearched_lower_bound = true;
            current.unsearched_files = current.unsearched_files.saturating_add(1);
            current.complete = false;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                current.walk_failures += 1;
                current.complete = false;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        current.walked_files += 1;
        if paths.len() == max_paths {
            current.path_catalog_truncated = true;
            current.unsearched_lower_bound = true;
            current.unsearched_files = current.unsearched_files.saturating_add(1);
            current.complete = false;
            break;
        }
        let relative = entry.path().strip_prefix(&store.root)?;
        paths.push(store::encode_path(relative));
    }
    Ok(paths)
}

fn attach_filename_handles(
    root: &Path,
    set: &mut ResultSet,
    deadline: &QueryDeadline,
    current: &mut coverage::CurrentCoverage,
) -> Result<()> {
    let mut hits = std::mem::take(&mut set.hits).into_iter();
    let mut retained = Vec::with_capacity(hits.len());
    while let Some(mut hit) = hits.next() {
        check_deadline(deadline)?;
        let relative = store::decode_path(&hit.path)?;
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
        if next_bytes > MAX_CURRENT_SOURCE_BYTES {
            current.unsearched_files = current.unsearched_files.saturating_add(1 + hits.count());
            current.complete = false;
            break;
        }
        let revision = crate::identity::ContentRevision::of(&bytes).to_string();
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

fn read_current_sources(
    store: &Store,
    paths: &[String],
    deadline: &QueryDeadline,
    current: &mut coverage::CurrentCoverage,
) -> Result<()> {
    for (index, encoded_path) in paths.iter().enumerate() {
        check_deadline(deadline)?;
        let relative = store::decode_path(encoded_path)?;
        let bytes = match source::read_contained(&store.root, &relative, MAX_CURRENT_FILE_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if ErrorCode::SourceExcluded.is_code_of(&error) => {
                store::current_source::mark_unavailable(store, encoded_path, "source_excluded", 0)?;
                current.excluded_files += 1;
                current.complete = false;
                continue;
            }
            Err(_) => {
                store::current_source::mark_unavailable(
                    store,
                    encoded_path,
                    "source_unavailable",
                    0,
                )?;
                current.read_failures += 1;
                current.complete = false;
                continue;
            }
        };
        if bytes.contains(&0) {
            store::current_source::mark_unavailable(store, encoded_path, "binary", bytes.len())?;
            current.excluded_files += 1;
            current.complete = false;
            continue;
        }
        let next_bytes = current.bytes_read.saturating_add(bytes.len());
        if next_bytes > MAX_CURRENT_SOURCE_BYTES {
            current.unsearched_files = current
                .unsearched_files
                .saturating_add(paths.len().saturating_sub(index));
            current.complete = false;
            break;
        }
        let extraction = crate::extract::extract(encoded_path, &bytes);
        let facts = extraction_fact_count(&extraction);
        let next_facts = current.facts_indexed.saturating_add(facts);
        if next_facts > MAX_CURRENT_SOURCE_FACTS {
            current.unsearched_files = current
                .unsearched_files
                .saturating_add(paths.len().saturating_sub(index));
            current.complete = false;
            break;
        }
        check_deadline(deadline)?;
        if !matches!(
            extraction.status.as_str(),
            "ok" | "parsed" | "complete" | "lexical_only" | "recovered"
        ) {
            current.parse_failures += 1;
            current.complete = false;
        }
        store::current_source::insert(store, encoded_path, &bytes, &extraction)?;
        current.read_files += 1;
        current.bytes_read = next_bytes;
        current.facts_indexed = next_facts;
    }
    Ok(())
}

fn live_coverage(set: &ResultSet, query: &Query) -> coverage::CurrentCoverage {
    let value = &set.coverage;
    let checked = value["live_checked_files"].as_u64().unwrap_or(0) as usize;
    let excluded = value["live_excluded_files"].as_u64().unwrap_or(0) as usize;
    let read_failures = value["live_read_failures"].as_u64().unwrap_or(0) as usize;
    let walk_failures = value["live_walk_failures"].as_u64().unwrap_or(0) as usize;
    let kind_filter = !query.kind.is_empty();
    let mut current = coverage::CurrentCoverage {
        walked_files: checked
            .saturating_add(excluded)
            .saturating_add(read_failures),
        read_files: checked,
        excluded_files: excluded,
        read_failures,
        walk_failures,
        unsearched_files: 0,
        complete: read_failures == 0 && walk_failures == 0 && excluded == 0 && !set.truncated,
        ..coverage::CurrentCoverage::default()
    };
    if kind_filter {
        current.unsearched_unknown = true;
        current.complete = false;
    }
    current
}

fn empty_set(
    current: coverage::CurrentCoverage,
    query: &Query,
    semantic_requested: bool,
    rerank_requested: bool,
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
        query,
        semantic_requested,
        rerank_requested,
    );
    set
}

fn current_relative_path(path: &str) -> Result<std::path::PathBuf> {
    let path = path.strip_prefix("./").unwrap_or(path);
    let relative = store::decode_path(path)?;
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        coded!(InvalidPath, "use one root-relative current file path")
    );
    Ok(relative)
}

fn extraction_fact_count(extraction: &crate::extract::Extraction) -> usize {
    extraction
        .definitions
        .len()
        .saturating_add(extraction.occurrences.len())
        .saturating_add(extraction.relationships.len())
}

fn validate_cache_exclusion(root: &Path, cache: &Path) -> Result<()> {
    ensure!(
        !root.starts_with(normalized_path(cache)),
        coded!(
            InvalidCache,
            "selected cache cannot contain the repository root"
        )
    );
    Ok(())
}

fn normalized_path(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|current| current.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
}

fn check_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        bail!(coded!(
            TimedOut,
            "stateless current-source query deadline expired"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod filename_handle_tests {
    use super::*;
    use crate::{output::OutputBudget, search::telemetry::RetrievalTrace};
    use std::{fs, time::Duration};

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
        let mut current = coverage::CurrentCoverage {
            complete: false,
            unsearched_files: 1,
            ..coverage::CurrentCoverage::default()
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
