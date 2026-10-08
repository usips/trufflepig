//! `show` for a home member answered through its parent's index: definitions
//! in changed files are re-extracted from worktree bytes, and a file whose
//! bytes cannot be extracted falls back to the parent index's stored bytes,
//! marked unverified. Reads never create or write an index.
use super::{
    HomeIndexPolicy, HomeIndexSource, ParentIndexView, WorktreeHashes, resolve_home_index,
    worktree_reads::unextractable,
};
use crate::{
    daemon::deadline::QueryDeadline,
    identity::{ByteSpan, ContentRevision},
    results::{Hit, ResultEntry},
    search::{InvocationDirectory, Query},
    source::{self, AcquiredSource, SourceSide, acquisition},
    store::Store,
    workspace::member_root::MemberRoot,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

/// How a parent-index `show` reached bytes other than the verified indexed ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParentRead {
    /// The same definition re-extracted from current worktree bytes.
    Reextracted,
    /// The parent index's stored bytes: the worktree file is binary or its
    /// extraction failed, so they are unverified against the worktree.
    Indexed,
}

impl ParentRead {
    /// Names the read's provenance in a `show` response's metadata.
    pub(crate) fn annotate(self, member: &str, metadata: &mut Value) {
        metadata["served_from"] = match self {
            Self::Reextracted => format!("{member} index; re-extracted in worktree"),
            Self::Indexed => {
                metadata["source"] = "parent_index".into();
                format!("{member} index; worktree file not extractable")
            }
        }
        .into();
    }
}

/// `show TARGET` on a member without a result handle: through its own index or,
/// while a worktree warms, its parent's ([`acquire_through_parent`]). With
/// neither published, `--no-daemon` indexes the member first; otherwise the read
/// answers `index_warming`.
pub(in crate::workspace) fn acquire_home_read(
    member: &MemberRoot,
    cache: &Path,
    explicit_cache: Option<&Path>,
    request: (&str, Option<SourceSide>, &InvocationDirectory),
    no_daemon: bool,
    deadline: &mut QueryDeadline,
) -> Result<(Store, AcquiredSource, Option<ParentRead>)> {
    let (target, side, origin) = request;
    let policy = HomeIndexPolicy::AwaitDaemon(std::time::Duration::ZERO);
    let mut resolved = resolve_home_index(member, cache, explicit_cache, policy, *deadline);
    if no_daemon
        && target.starts_with("sym:")
        && matches!(resolved, HomeIndexSource::Warming { .. })
    {
        // `--no-daemon` indexes in the foreground, outside the query deadline.
        deadline.pause_during(|| Store::open(&member.root, cache)?.index())?;
        let policy = HomeIndexPolicy::IndexInline;
        resolved = resolve_home_index(member, cache, explicit_cache, policy, *deadline);
    }
    match resolved {
        HomeIndexSource::Own(store) => {
            let source = acquisition::acquire(&store, target, side, origin)?;
            Ok((store, source, None))
        }
        HomeIndexSource::Parent(mut view) => {
            let (source, read) = acquire_through_parent(&mut view, target, side, origin)?;
            Ok((view.store, source, read))
        }
        HomeIndexSource::Warming { .. } if !target.starts_with("sym:") => {
            acquire_current_path(member, cache, target, side, origin)
        }
        HomeIndexSource::Warming { reason } => anyhow::bail!(
            "index_warming: {} has no published index yet{}; retry shortly",
            member.display_name(),
            reason
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        ),
        HomeIndexSource::Unavailable { .. } if !target.starts_with("sym:") => {
            acquire_current_path(member, cache, target, side, origin)
        }
        HomeIndexSource::Unavailable { reason } => anyhow::bail!(reason),
    }
}

/// Reads an explicit path directly when no published graph can answer it.
fn acquire_current_path(
    member: &MemberRoot,
    cache: &Path,
    target: &str,
    side: Option<SourceSide>,
    origin: &InvocationDirectory,
) -> Result<(Store, AcquiredSource, Option<ParentRead>)> {
    std::fs::create_dir_all(cache)?;
    let store = Store::unpublished(&member.root, cache)?;
    let source = acquisition::acquire(&store, target, side, origin)?;
    Ok((store, source, None))
}

/// `show TARGET` through a parent index. A `sym:` read ranks the parent's
/// definitions brought up to the worktree ([`ParentFallback::worktree_symbols`])
/// and shows the best as `acquisition::read_symbol` does; a shown row whose
/// file changed is re-read from worktree bytes ([`reextracted_entry`]).
pub(crate) fn acquire_through_parent(
    view: &mut ParentIndexView,
    target: &str,
    side: Option<SourceSide>,
    origin: &InvocationDirectory,
) -> Result<(AcquiredSource, Option<ParentRead>)> {
    // With divergent files a `sym:` read always consults the worktree, so it
    // agrees with `sym:` search even when the parent's definition verifies.
    let consult_worktree = target.starts_with("sym:")
        && (!view.fallback.divergence.paths.is_empty() || !view.fallback.divergence.complete);
    if !consult_worktree {
        let error = match acquisition::acquire(&view.store, target, side, origin) {
            Ok(source) => return Ok((source, None)),
            Err(error) => error,
        };
        let message = error.to_string();
        if !target.starts_with("sym:")
            || !(message.starts_with("stale_source") || message.starts_with("no_definition"))
        {
            return Err(error);
        }
    }
    let store = &view.store;
    let query = Query::parse(target)?;
    let mut found = crate::search::definitions(store, &query, origin)?;
    let mut hashes = WorktreeHashes::default();
    let parent = std::mem::take(&mut found.hits);
    let symbols = view
        .fallback
        .worktree_symbols(store, &query, parent, origin, &mut hashes)?;
    anyhow::ensure!(
        !symbols.incomplete || !symbols.hits.is_empty(),
        "worktree_search_incomplete: exact symbol lookup could not inspect every changed file"
    );
    found.hits = symbols.hits;
    let read = std::cell::Cell::new(None);
    let source = acquisition::read_symbol(store, &query, found, origin, |handle, hit| {
        let reextracted = hit.provenance.as_deref() == Some("reextracted");
        let entry = ResultEntry::LiveSource(hit);
        match acquisition::acquire_entry(store, handle, entry.clone(), None) {
            Ok(source) => {
                read.set(reextracted.then_some(ParentRead::Reextracted));
                Ok(source)
            }
            Err(error) if error.to_string().starts_with("stale_source") => {
                let (source, how) = reextracted_entry(store, handle, &entry)?.ok_or(error)?;
                read.set(Some(how));
                Ok(source)
            }
            Err(error) => Err(error),
        }
    })?;
    Ok((source, read.get()))
}

/// Re-reads a parent-index definition hit whose worktree file changed: the
/// same-name, same-kind definition nearest its recorded line from current
/// bytes, or the index's own bytes when the worktree file cannot be extracted.
/// `None` when the worktree file no longer defines it.
pub(crate) fn reextracted_entry(
    store: &Store,
    handle: &str,
    entry: &ResultEntry,
) -> Result<Option<(AcquiredSource, ParentRead)>> {
    let ResultEntry::LiveSource(hit) = entry else {
        return Ok(None);
    };
    if matches!(hit.kind.as_str(), "region" | "file" | "source_read") {
        return Ok(None);
    }
    let root = &store.root;
    let current = match source::reextract_definition(root, hit) {
        Ok(Some(current)) => current,
        Ok(None) => return Ok(None),
        // Only a hit carrying an indexed revision has index bytes to fall back to.
        Err(error) if unextractable(&error) => {
            let indexed = indexed_source(store, handle, hit).ok();
            return Ok(indexed.map(|source| (source, ParentRead::Indexed)));
        }
        Err(error) => return Err(error),
    };
    let source = AcquiredSource {
        bytes: current.bytes,
        path: current.path,
        revision: current.revision,
        span: current.span,
        verified: true,
        handle: handle.to_owned(),
        side: None,
        historical: None,
        definitions: None,
        freshness: None,
    };
    Ok(Some((source, ParentRead::Reextracted)))
}

/// The parent index's stored bytes for `hit`, unverified against the worktree.
fn indexed_source(store: &Store, handle: &str, hit: &Hit) -> Result<AcquiredSource> {
    let revision = hit
        .revision
        .as_deref()
        .context("source_excluded: no indexed source revision")?;
    let bytes: Vec<u8> = store.conn.query_row(
        "SELECT bytes FROM contents WHERE revision=?1",
        [revision],
        |row| row.get(0),
    )?;
    let span = ByteSpan::new(hit.start, hit.end)?.validate(bytes.len())?;
    Ok(AcquiredSource {
        bytes,
        path: hit.path.clone(),
        revision: ContentRevision::parse(revision)?,
        span,
        verified: false,
        handle: handle.to_owned(),
        side: None,
        historical: None,
        definitions: None,
        freshness: None,
    })
}
