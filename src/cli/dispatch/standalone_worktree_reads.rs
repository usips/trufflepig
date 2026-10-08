//! Standalone linked-worktree read routing and immutable parent-result ownership.
use super::Arguments;
use crate::{
    daemon::deadline::QueryDeadline,
    output::OutputBudget,
    results, search, source,
    store::{Store, is_index_cannot_open, is_index_warming},
};
use anyhow::{Context, Result, bail, ensure};
use std::{os::unix::fs::MetadataExt, path::Path};

pub(super) fn retrieve_read(
    root: &Path,
    cache: &Path,
    store: &Store,
    mut fallback: Option<crate::workspace::ParentFallback>,
    verb: &str,
    options: &Arguments,
    session: &mut crate::semantic::SemanticSession,
    context: &crate::diagnostics::RequestContext,
    log_queue: Option<&crate::diagnostics::DiagnosticQueue>,
    preparation_manager: Option<&crate::semantic::preparation::PreparationManager>,
    budget: &OutputBudget,
) -> Result<String> {
    let parent_identity = fallback
        .as_ref()
        .map(|fallback| crate::workspace::ParentIndexIdentity::capture(store, fallback))
        .transpose()?;
    let words = &options.words[1..];
    let request_words = &options.words;
    let text = words.join(" ");
    let query = if verb == "search" && !text.starts_with("refs:") {
        Some(search::Query::parse(&text)?)
    } else {
        None
    };
    let mut map_miss = None;
    let mut trace = if options.diagnostics == "off" {
        search::telemetry::RetrievalTrace::disabled()
    } else {
        search::telemetry::RetrievalTrace::default()
    };
    let mut set = match verb {
        "refs" => search::references(store, &search::reference_query(&text)?)?,
        "map" => {
            let found = search::map(store, words.first().map_or("", String::as_str))?;
            map_miss = search::map_miss(&found).map(str::to_owned);
            found
        }
        "search" if text.starts_with("refs:") => {
            search::references(store, &search::reference_query(&text)?)?
        }
        "search" => {
            let query = query.as_ref().expect("search query parsed above");
            let preparation_error =
                if options.sem && !options.no_daemon && !query.exact && !query.regex {
                    preparation_manager.and_then(|manager| manager.schedule(root, cache).err())
                } else {
                    None
                };
            let mut result = search::search_with_session(
                store,
                &query,
                options.sem,
                options.rerank,
                store.index_cache(),
                session,
                &mut trace,
            );
            if let (Some(error), Ok(set)) = (preparation_error, &mut result) {
                set.coverage["semantic_preparation_error"] = error.to_string().into();
            }
            let mut event = crate::diagnostics::RequestEvent::new(
                context.clone(),
                crate::diagnostics::Operation::Search,
                if result.is_ok() {
                    crate::diagnostics::Outcome::Success
                } else {
                    crate::diagnostics::Outcome::Failure
                },
            );
            event.stage = crate::diagnostics::EventStage::Server;
            event.retrieval = Some(trace);
            if options.diagnostics != "off" {
                if let Some(queue) = log_queue {
                    queue.record(event);
                } else {
                    crate::diagnostics::best_effort_record(
                        cache,
                        crate::cli::emission::diagnostics_mode(options),
                        event,
                    );
                }
            }
            result?
        }
        _ => unreachable!(),
    };
    if let Some(mut fallback) = fallback.take() {
        let origin = search::InvocationDirectory::within(store.root.as_path(), root);
        let empty_query = search::Query::default();
        let request_query = query.as_ref().unwrap_or(&empty_query);
        let mut hashes = crate::workspace::WorktreeHashes::default();
        let changed = fallback.complete_answer(
            store,
            (verb, request_words, request_query, &origin),
            &mut set,
            &mut hashes,
        )?;
        let differing = fallback.check_hits(&store.root, &set.hits, &mut hashes, changed);
        let differing_hits = set.hits.iter().filter(|hit| differing.flags(hit)).count();
        for hit in &mut set.hits {
            hit.differs = differing.flags(hit);
        }
        fallback.describe(&mut set.coverage, &differing, differing_hits);
        let identity = parent_identity.expect("parent fallback captured above");
        identity.validate_publication(store)?;
        ensure!(
            set.generation == identity.generation,
            "stale_result: parent index changed during retrieval; search again"
        );
        set.coverage["parent_index"] = serde_json::to_value(identity)?;
    }
    if verb == "map"
        && let Some(miss) = search::map_miss(&set)
    {
        map_miss.get_or_insert_with(|| miss.to_owned());
        bail!(map_miss.expect("map miss was just set"));
    }
    let id = results::save(store, set)?;
    results::page(store, &id, 0, options.limit, budget)
}

pub(super) fn show_read(
    root: &Path,
    cache: &Path,
    store: Store,
    fallback: Option<crate::workspace::ParentFallback>,
    options: &Arguments,
    budget: &OutputBudget,
    deadline: QueryDeadline,
) -> Result<String> {
    let target = source::acquisition::show_target(&options.words)
        .context("usage: command requires an explicit argument")?;
    let side = options
        .side
        .as_deref()
        .map(source::SourceSide::parse)
        .transpose()?;
    let origin = search::InvocationDirectory::within(store.root.as_path(), root);
    if let Some(fallback) = fallback.filter(|_| target.starts_with("sym:")) {
        let mut view = crate::workspace::ParentIndexView { store, fallback };
        let identity = crate::workspace::ParentIndexIdentity::capture(&view.store, &view.fallback)?;
        let (mut source, read) =
            crate::workspace::acquire_through_parent(&mut view, &target, side, &origin)?;
        source.handle = save_parent_identity(&view.store, &source.handle, &identity)?;
        let mut metadata = serde_json::json!({"served_from":identity.served_from.clone()});
        if let Some(read) = read {
            read.annotate(
                identity
                    .served_from
                    .strip_suffix(" index")
                    .unwrap_or(&identity.served_from),
                &mut metadata,
            );
        }
        return source::render_owned(source, budget, &metadata);
    }
    if let Some(handle) = target_handle(&target) {
        let saved_parent = parent_result_store(root, cache, &store, &handle, deadline)?;
        let read_store = saved_parent.as_ref().map_or(&store, |(parent, _)| parent);
        let identity = saved_parent.as_ref().map(|(_, identity)| identity);
        if identity.is_none() {
            let generation = stored_generation(&store, &handle)?;
            ensure!(
                generation == 0 || store.index_root() == store.root,
                "stale_result: parent-index origin is missing; search again"
            );
        }
        let acquired = match source::acquisition::acquire(read_store, &target, side, &origin) {
            Err(error) if identity.is_some() && error.to_string().starts_with("stale_source:") => {
                let (generation, entry) = results::entry(read_store, &handle)?;
                let _ = generation;
                let (mut current, read) =
                    crate::workspace::reextracted_entry(read_store, &handle, &entry)?
                        .ok_or(error)?;
                if let Some(offset) = target_offset(&target)? {
                    ensure!(
                        offset >= current.span.start && offset <= current.span.end,
                        "invalid_cursor: offset outside original source range"
                    );
                    current.span.start = offset;
                }
                let mut metadata = serde_json::json!({
                    "served_from": identity.as_ref().expect("checked above").served_from,
                });
                read.annotate(
                    identity
                        .expect("checked above")
                        .served_from
                        .strip_suffix(" index")
                        .unwrap_or("parent"),
                    &mut metadata,
                );
                return source::render_owned(current, budget, &metadata);
            }
            acquired => acquired?,
        };
        let mut metadata = serde_json::json!({});
        if let Some(identity) = identity {
            metadata["served_from"] = identity.served_from.clone().into();
        }
        return source::render_owned(acquired, budget, &metadata);
    }
    source::show_with_side(&store, &target, side, budget)
}

pub(super) fn context_read(
    root: &Path,
    cache: &Path,
    store: &Store,
    handle: &str,
    deadline: QueryDeadline,
    budget: &OutputBudget,
) -> Result<String> {
    ensure!(
        !handle.starts_with("read:"),
        "invalid_handle: context requires a live result handle"
    );
    let handle =
        target_handle(handle).context("invalid_handle: context requires a live result handle")?;
    ensure!(
        stored_generation(store, &handle)? > 0,
        "stale_result: current-path handles have no indexed context; search again"
    );
    match parent_result_store(root, cache, store, &handle, deadline)? {
        Some((parent, _identity)) => search::context(&parent, &handle, budget),
        None => {
            ensure!(
                store.index_root() == store.root,
                "stale_result: parent-index origin is missing; search again"
            );
            search::context(store, &handle, budget)
        }
    }
}

fn parent_result_store(
    root: &Path,
    cache: &Path,
    store: &Store,
    handle: &str,
    deadline: QueryDeadline,
) -> Result<Option<(Store, crate::workspace::ParentIndexIdentity)>> {
    parent_result_store_for_set(root, cache, store, &result_set_id(handle)?, deadline)
}

fn result_set_id(handle: &str) -> Result<String> {
    let parsed: crate::identity::ResultHandle = handle.parse()?;
    Ok(parsed.set.simple().to_string())
}

fn stored_generation(store: &Store, handle: &str) -> Result<i64> {
    Ok(results::load_entries(store, &result_set_id(handle)?)?.generation)
}

fn parent_result_store_for_set(
    root: &Path,
    cache: &Path,
    store: &Store,
    set_id: &str,
    deadline: QueryDeadline,
) -> Result<Option<(Store, crate::workspace::ParentIndexIdentity)>> {
    let set = results::load_entries(store, set_id)?;
    let Some(value) = set.coverage.get("parent_index").cloned() else {
        return Ok(None);
    };
    let identity: crate::workspace::ParentIndexIdentity = serde_json::from_value(value)
        .context("stale_result: parent-index identity is invalid; search again")?;
    let member = validate_parent_worktree(root, &identity)?;
    let parent_root = member.member.root;
    let default_cache = crate::cli::cache_path(root, None)?;
    ensure!(
        cache == default_cache,
        "stale_result: saved parent index requires the default worktree cache; search again"
    );
    let parent_cache = crate::cli::cache_path(&parent_root, None)?;
    let parent = Store::open_read(&parent_root, &parent_cache, deadline).map_err(|_| {
        anyhow::anyhow!("stale_result: saved parent index is no longer available; search again")
    })?;
    identity.validate_publication(&parent)?;
    Ok(Some((parent.reading_from(root, cache)?, identity)))
}

fn validate_parent_worktree(
    root: &Path,
    identity: &crate::workspace::ParentIndexIdentity,
) -> Result<crate::workspace::member_root::MemberRoot> {
    let member = crate::workspace::member_root::standalone_worktree(root)
        .context("stale_result: linked worktree identity changed; search again")?;
    let worktree_metadata = member
        .root
        .metadata()
        .context("stale_result: linked worktree is unavailable; search again")?;
    ensure!(
        crate::store::encode_path(&member.root) == identity.worktree_root
            && worktree_metadata.dev() == identity.worktree_device
            && worktree_metadata.ino() == identity.worktree_inode,
        "stale_result: linked worktree was replaced; search again"
    );
    let parent_root = crate::store::decode_path(&identity.root)?
        .canonicalize()
        .context("stale_result: parent checkout is unavailable; search again")?;
    ensure!(
        parent_root == member.member.root,
        "stale_result: saved parent index belongs to another checkout; search again"
    );
    Ok(member)
}

pub(super) fn more_read(
    store: &Store,
    cursor: &str,
    limit: usize,
    budget: &OutputBudget,
) -> Result<String> {
    let cursor: crate::identity::ResultCursor = cursor.parse()?;
    let set_id = cursor.set.simple().to_string();
    let set = results::load_entries(store, &set_id)?;
    if let Some(value) = set.coverage.get("parent_index") {
        let identity: crate::workspace::ParentIndexIdentity = serde_json::from_value(value.clone())
            .context("stale_result: parent-index identity is invalid; search again")?;
        // Page snapshots survive parent republishes, but their worktree cache
        // belongs to the checkout that created the result set.
        validate_parent_worktree(&store.root, &identity)?;
    }
    results::more(store, &cursor.to_string(), limit, budget)
}

fn save_parent_identity(
    store: &Store,
    handle: &str,
    identity: &crate::workspace::ParentIndexIdentity,
) -> Result<String> {
    let parsed: crate::identity::ResultHandle = handle.parse()?;
    let mut set = results::load_entries(store, &parsed.set.simple().to_string())?;
    identity.validate_publication(store)?;
    ensure!(
        set.generation == identity.generation,
        "stale_result: parent index changed during retrieval; search again"
    );
    set.coverage["parent_index"] = serde_json::to_value(identity)?;
    let new_set =
        results::save_entries(store, set.generation, set.coverage, set.hits, set.truncated)?;
    Ok(format!("{new_set}:{}", parsed.ordinal))
}

fn target_offset(target: &str) -> Result<Option<usize>> {
    let Some(cursor) = target.strip_prefix("read:") else {
        return Ok(None);
    };
    let (_, offset) = cursor
        .rsplit_once('@')
        .context("invalid_cursor: expected read:HANDLE@BYTE_OFFSET")?;
    Ok(Some(
        offset
            .parse()
            .context("invalid_cursor: invalid byte offset")?,
    ))
}

/// New parent-index reads are limited to search, map, and symbol lookup.
/// `refs` and `search refs:` retain their existing index-only behavior.
pub(super) fn can_use_parent(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    error: &anyhow::Error,
) -> bool {
    if !(is_index_warming(error) || is_index_cannot_open(error)) {
        return false;
    }
    let supported = match options.words.first().map(String::as_str) {
        Some("search") => !options.words[1..].join(" ").starts_with("refs:"),
        Some("map") => true,
        Some("show") => source::acquisition::show_target(&options.words)
            .is_some_and(|target| target.starts_with("sym:")),
        _ => false,
    };
    supported && crate::cli::cache_path(root, None).is_ok_and(|default| cache == default)
}

pub(super) fn can_read_without_index(options: &Arguments) -> bool {
    match options.words.first().map(String::as_str) {
        Some("more" | "ctx") => true,
        Some("show") => source::acquisition::show_target(&options.words)
            .is_some_and(|target| !target.starts_with("sym:")),
        _ => false,
    }
}

fn target_handle(target: &str) -> Option<String> {
    let target = target.strip_prefix("read:").unwrap_or(target);
    let target = target.rsplit_once('@').map_or(target, |(handle, _)| handle);
    let target = match target.rsplit_once(':') {
        Some((handle, side @ ("before" | "after"))) => {
            let _ = side;
            handle
        }
        _ => target,
    };
    target
        .parse::<crate::identity::ResultHandle>()
        .ok()
        .map(|handle| handle.to_string())
}
