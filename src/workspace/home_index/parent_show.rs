//! `show` for a home member answered through its parent's index: definitions
//! in changed files are re-extracted from worktree bytes, and a file whose
//! bytes cannot be extracted falls back to the parent index's stored bytes,
//! marked unverified. Reads never create or write an index.
use super::{
    HomeIndexPolicy, HomeIndexSource, ParentIndexView, resolve_home_index,
    worktree_reads::MAX_REEXTRACTED_FILES,
};
use crate::{
    daemon::deadline::QueryDeadline,
    identity::{ByteSpan, ContentRevision},
    results::{self, Hit, ResultEntry},
    search::Query,
    source::{self, AcquiredSource, SourceSide, acquisition},
    store::Store,
    workspace::member_root::MemberRoot,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

/// Other same-named definitions listed after a re-extracted `sym:` read.
const SYMBOL_ALTERNATIVES: usize = 5;

/// How a parent-index `show` reached bytes other than the verified indexed ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum ParentRead {
    /// The same definition re-extracted from current worktree bytes.
    Reextracted,
    /// The parent index's stored bytes: the worktree file is binary or its
    /// extraction failed, so they are unverified against the worktree.
    Indexed,
}

impl ParentRead {
    /// Names the read's provenance in a `show` response's metadata.
    pub(in crate::workspace) fn annotate(self, member: &str, metadata: &mut Value) {
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

/// Worktree bytes exist but their definitions cannot be known.
fn unextractable(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.starts_with("source_excluded") || message.starts_with("extraction_unavailable")
}

/// `show TARGET` on a member without a result handle: through its own index or,
/// while a worktree warms, its parent's ([`acquire_through_parent`]). With
/// neither published, `--no-daemon` indexes the member first; otherwise the read
/// answers `index_warming`.
pub(in crate::workspace) fn acquire_home_read(
    member: &MemberRoot,
    cache: &Path,
    explicit_cache: Option<&Path>,
    target: &str,
    side: Option<SourceSide>,
    no_daemon: bool,
    deadline: &mut QueryDeadline,
) -> Result<(Store, AcquiredSource, Option<ParentRead>)> {
    let policy = HomeIndexPolicy::AwaitDaemon(std::time::Duration::ZERO);
    let mut resolved = resolve_home_index(member, cache, explicit_cache, policy, *deadline);
    if no_daemon && matches!(resolved, HomeIndexSource::Warming { .. }) {
        // `--no-daemon` indexes in the foreground, outside the query deadline.
        deadline.pause_during(|| Store::open(&member.root, cache)?.index())?;
        let policy = HomeIndexPolicy::IndexInline;
        resolved = resolve_home_index(member, cache, explicit_cache, policy, *deadline);
    }
    match resolved {
        HomeIndexSource::Own(store) => {
            let source = acquisition::acquire(&store, target, side)?;
            Ok((store, source, None))
        }
        HomeIndexSource::Parent(view) => {
            let (source, read) = acquire_through_parent(&view, target, side)?;
            Ok((view.store, source, read))
        }
        HomeIndexSource::Warming { reason } => anyhow::bail!(
            "index_warming: {} has no published index yet{}; retry shortly",
            member.display_name(),
            reason
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        ),
        HomeIndexSource::Unavailable { reason } => anyhow::bail!(reason),
    }
}

/// `show TARGET` through a parent index. A `sym:` read whose definition lives in
/// a changed file is re-extracted from worktree bytes (or read from the index,
/// unverified, when they cannot be extracted); one that exists only in the
/// worktree is found in changed files.
fn acquire_through_parent(
    view: &ParentIndexView,
    target: &str,
    side: Option<SourceSide>,
) -> Result<(AcquiredSource, Option<ParentRead>)> {
    let error = match acquisition::acquire(&view.store, target, side) {
        Ok(source) => return Ok((source, None)),
        Err(error) => error,
    };
    let message = error.to_string();
    if !target.starts_with("sym:")
        || !(message.starts_with("stale_source") || message.starts_with("no_definition"))
    {
        return Err(error);
    }
    let query = Query::parse(target)?;
    let found = crate::search::definitions(&view.store, &query)?;
    let fallback = &view.fallback;
    let mut hits = Vec::with_capacity(found.hits.len());
    let mut first_unverified = false;
    for (rank, hit) in found.hits.into_iter().enumerate() {
        // An incomplete divergence cannot clear a file, so leading hits re-extract.
        let changed = fallback.differs(&hit.path)
            || (!fallback.divergence.complete && rank < MAX_REEXTRACTED_FILES);
        if !changed {
            hits.push(hit);
            continue;
        }
        let root = &view.store.root;
        match source::reextract_definition(root, &hit.path, &hit.name, &hit.kind, hit.start_line) {
            Ok(Some(current)) => hits.push(current.hit()),
            Ok(None) => {}
            Err(error) if unextractable(&error) => {
                first_unverified |= hits.is_empty();
                hits.push(hit);
            }
            Err(_) => {}
        }
    }
    if hits.is_empty() {
        hits = fallback.worktree_definitions(&view.store, &query);
    }
    if hits.is_empty() {
        return Err(error);
    }
    let total = hits.len();
    let alternatives = hits[1..]
        .iter()
        .take(SYMBOL_ALTERNATIVES)
        .map(|hit| {
            format!(
                "{}:{}-{} {}",
                hit.path, hit.start_line, hit.end_line, hit.kind
            )
        })
        .collect();
    let first = hits[0].clone();
    let set = results::save_entries(
        &view.store,
        found.generation,
        found.coverage,
        hits.into_iter().map(ResultEntry::LiveSource).collect(),
        found.truncated,
    )?;
    let handle = format!("{set}:1");
    let (mut source, read) = if first_unverified {
        let source = indexed_source(&view.store, &handle, &first)?;
        (source, Some(ParentRead::Indexed))
    } else {
        let read =
            (first.provenance.as_deref() == Some("reextracted")).then_some(ParentRead::Reextracted);
        let entry = ResultEntry::LiveSource(first);
        (
            acquisition::acquire_entry(&view.store, &handle, entry, None)?,
            read,
        )
    };
    source.definitions = Some((total, alternatives));
    Ok((source, read))
}

/// Re-reads a parent-index definition hit whose worktree file changed: the
/// same-name, same-kind definition nearest its recorded line from current
/// bytes, or the index's own bytes when the worktree file cannot be extracted.
/// `None` when the worktree file no longer defines it.
pub(in crate::workspace) fn reextracted_entry(
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
    let current =
        match source::reextract_definition(root, &hit.path, &hit.name, &hit.kind, hit.start_line) {
            Ok(Some(current)) => current,
            Ok(None) => return Ok(None),
            Err(error) if unextractable(&error) => {
                return Ok(Some((
                    indexed_source(store, handle, hit)?,
                    ParentRead::Indexed,
                )));
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
    })
}
