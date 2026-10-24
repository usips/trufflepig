//! `show` uses a member's own publication or a linked worktree's parent index.
//! No-daemon symbol reads can seed the worktree cache and make one bounded
//! indexing attempt. A parent view selected for the current response remains
//! the source even when that attempt publishes the worktree's own index.
use super::{
    HomeIndexPolicy, HomeIndexSource, ParentIndexView, WorktreeHashes, is_reextracted_worktree_hit,
    resolve_home_index, worktree_reads::unextractable,
};
use crate::{
    coded_error::{ErrorCode, error_code},
    identity::{ByteSpan, ContentRevision},
    query_deadline::QueryDeadline,
    results::{Hit, ResultEntry},
    search::{InvocationDirectory, Query},
    source::{self, AcquiredSource, SourceSide, acquisition},
    store::Store,
    workspace::member_root::MemberRoot,
};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
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

/// Reads `show TARGET` through the member's own publication or a warming
/// worktree's parent publication. No-daemon symbol reads make one bounded
/// inline indexing attempt while retaining any parent view already selected.
pub(in crate::workspace) fn acquire_home_read(
    member: &MemberRoot,
    cache: &Path,
    explicit_cache: Option<&Path>,
    request: (&str, Option<SourceSide>, &InvocationDirectory),
    no_daemon: bool,
    deadline: &QueryDeadline,
    maintenance_deadline: &QueryDeadline,
) -> Result<(Store, AcquiredSource, Option<ParentRead>)> {
    let (target, side, origin) = request;
    let policy = HomeIndexPolicy::AwaitDaemon(std::time::Duration::ZERO);
    let mut resolved = resolve_home_index(member, cache, explicit_cache, policy, deadline.clone());
    if no_daemon && target.starts_with("sym:") {
        resolved = match resolved {
            HomeIndexSource::Own(store) => HomeIndexSource::Own(store),
            HomeIndexSource::Parent(view) => {
                let _ =
                    maintain_worktree_index(member, cache, deadline, maintenance_deadline, false)?;
                HomeIndexSource::Parent(view)
            }
            HomeIndexSource::Warming { .. } => {
                let _ =
                    maintain_worktree_index(member, cache, deadline, maintenance_deadline, true)?;
                resolve_home_index(member, cache, explicit_cache, policy, deadline.clone())
            }
            unavailable @ HomeIndexSource::Unavailable { .. } => unavailable,
        };
    }
    match resolved {
        HomeIndexSource::Own(store) => {
            let source = acquisition::acquire(&store, target, side, origin)?;
            Ok((store, source, None))
        }
        HomeIndexSource::Parent(mut view) => {
            ensure!(
                !deadline.expired(),
                coded!(TimedOut, "query deadline expired")
            );
            let (source, read) = acquire_through_parent(&mut view, target, side, origin, deadline)?;
            Ok((view.store, source, read))
        }
        HomeIndexSource::Warming { .. } if !target.starts_with("sym:") => {
            acquire_current_path(member, cache, target, side, origin, deadline)
        }
        HomeIndexSource::Warming { reason } => anyhow::bail!(coded!(
            IndexWarming,
            "{} has no published index yet{}; retry shortly",
            member.display_name(),
            reason
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        )),
        HomeIndexSource::Unavailable { .. } if !target.starts_with("sym:") => {
            acquire_current_path(member, cache, target, side, origin, deadline)
        }
        HomeIndexSource::Unavailable { error } => Err(error),
    }
}

fn maintain_worktree_index(
    member: &MemberRoot,
    cache: &Path,
    deadline: &QueryDeadline,
    maintenance_deadline: &QueryDeadline,
    wait_for_writer: bool,
) -> Result<Option<crate::store::Coverage>> {
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    if maintenance_deadline.expired() {
        return Ok(None);
    }
    let attempt = Store::open_for_reconcile(&member.root, cache, maintenance_deadline.clone())
        .and_then(|mut store| {
            if wait_for_writer {
                store.index_until(maintenance_deadline.clone())
            } else {
                store.index_available_until(maintenance_deadline.clone())
            }
        });
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    match attempt {
        Ok(coverage) => Ok(coverage),
        Err(error) => {
            let error = maintenance_deadline.classify(error);
            if crate::coded_error::ErrorCode::TimedOut.is_code_of(&error)
                && maintenance_deadline.expired()
            {
                ensure!(
                    !deadline.expired(),
                    coded!(TimedOut, "query deadline expired")
                );
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

/// Reads an explicit path directly when no published graph can answer it.
fn acquire_current_path(
    member: &MemberRoot,
    cache: &Path,
    target: &str,
    side: Option<SourceSide>,
    origin: &InvocationDirectory,
    deadline: &QueryDeadline,
) -> Result<(Store, AcquiredSource, Option<ParentRead>)> {
    std::fs::create_dir_all(cache)?;
    let store = Store::unpublished(&member.root, cache)?.with_query_deadline(deadline.clone());
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
    deadline: &QueryDeadline,
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
        let answerable_from_worktree = matches!(
            error_code(&error),
            Some(ErrorCode::StaleSource | ErrorCode::NoDefinition)
        );
        if !target.starts_with("sym:") || !answerable_from_worktree {
            return Err(error);
        }
    }
    let store = &view.store;
    let query = Query::parse(target)?;
    let mut found = crate::search::definitions(store, &query, origin)?;
    let mut hashes = WorktreeHashes::default();
    let parent = std::mem::take(&mut found.hits);
    let symbols =
        view.fallback
            .worktree_symbols(store, &query, parent, origin, &mut hashes, deadline)?;
    anyhow::ensure!(
        !symbols.incomplete || !symbols.hits.is_empty(),
        coded!(
            WorktreeSearchIncomplete,
            "exact symbol lookup could not inspect every changed file"
        )
    );
    found.hits = symbols.hits;
    let read = std::cell::Cell::new(None);
    let source = acquisition::read_symbol(store, &query, found, origin, |handle, hit| {
        let reextracted = is_reextracted_worktree_hit(&hit);
        let entry = ResultEntry::LiveSource(hit);
        match acquisition::acquire_entry(store, handle, entry.clone(), None) {
            Ok(source) => {
                read.set(reextracted.then_some(ParentRead::Reextracted));
                Ok(source)
            }
            Err(error) if ErrorCode::StaleSource.is_code_of(&error) => {
                let (source, how) = reextracted_entry(store, handle, &entry)?.ok_or(error)?;
                read.set(Some(how));
                Ok(source)
            }
            Err(error) => Err(error),
        }
    })?;
    Ok((source, read.get()))
}

/// Re-reads a parent-index hit whose worktree file changed: a whole-file hit as
/// the current file, a definition as the same-name, same-kind definition
/// nearest its recorded line, or the index's own bytes when the worktree file
/// cannot be extracted. `None` when the worktree file no longer defines it.
pub(crate) fn reextracted_entry(
    store: &Store,
    handle: &str,
    entry: &ResultEntry,
) -> Result<Option<(AcquiredSource, ParentRead)>> {
    let ResultEntry::LiveSource(hit) = entry else {
        return Ok(None);
    };
    let root = &store.root;
    if names_whole_file(hit) {
        let bytes = source::read_contained(
            root,
            &crate::path_codec::decode_path(&hit.path)?,
            source::MAX_READ_BYTES,
        )?;
        let source = AcquiredSource {
            revision: ContentRevision::of(&bytes),
            span: ByteSpan::new(0, bytes.len())?,
            bytes,
            path: hit.path.clone(),
            verified: true,
            handle: handle.to_owned(),
            side: None,
            historical: None,
            definitions: None,
            freshness: None,
        };
        return Ok(Some((source, ParentRead::Reextracted)));
    }
    if matches!(hit.kind.as_str(), "region" | "source_read") {
        return Ok(None);
    }
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

/// A file hit, or the whole-file module definition the index names by path.
fn names_whole_file(hit: &Hit) -> bool {
    hit.kind == "file" || (hit.kind == "module" && hit.name == hit.path && hit.container.is_none())
}

/// The parent index's stored bytes for `hit`, unverified against the worktree.
fn indexed_source(store: &Store, handle: &str, hit: &Hit) -> Result<AcquiredSource> {
    let (revision, bytes): (String, Vec<u8>) = store.conn.query_row(
        "SELECT f.revision,c.bytes FROM files f JOIN contents c ON c.revision=f.revision
         WHERE f.path=?1 AND f.revision IS NOT NULL",
        [&hit.path],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let span = if hit.revision.as_deref() == Some(revision.as_str()) {
        ByteSpan::new(hit.start, hit.end)?.validate(bytes.len())?
    } else {
        let definition = store
            .conn
            .query_row(
                "SELECT d.start,d.end FROM definitions d JOIN files f ON f.id=d.file_id
                 WHERE f.path=?1 AND d.name=?2 AND d.kind=?3
                 ORDER BY ABS(d.start-?4),d.start LIMIT 1",
                params![hit.path, hit.name, hit.kind, hit.start as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as usize,
                        row.get::<_, i64>(1)? as usize,
                    ))
                },
            )
            .optional()?;
        match definition {
            Some((start, end)) => ByteSpan::new(start, end)?.validate(bytes.len())?,
            None => ByteSpan::new(0, bytes.len())?,
        }
    };
    Ok(AcquiredSource {
        bytes,
        path: hit.path.clone(),
        revision: ContentRevision::parse(&revision)?,
        span,
        verified: false,
        handle: handle.to_owned(),
        side: None,
        historical: None,
        definitions: None,
        freshness: None,
    })
}
