//! Acquire and verify one immutable identity before rendering its remaining byte range.
use super::{MAX_READ_BYTES, read_contained};
use crate::{
    results::{self, HistoricalSource, ResultEntry},
    search::InvocationDirectory,
    store::Store,
};
use anyhow::{Result, bail, ensure};
use std::path::Component;
use tp_core::coded_error::CodedContext;
use tp_core::identity::{ByteSpan, ContentRevision, ResultHandle};

mod path_read;
pub(crate) use path_read::{cursor_matches_root, stateless_hit_handle};
mod symbol;
#[cfg(test)]
mod tests;

pub(crate) use symbol::{SymbolSelection, read_symbol, show_target};
mod stale_symbol;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceSide {
    Before,
    After,
}
impl SourceSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "before" => Ok(Self::Before),
            "after" => Ok(Self::After),
            _ => bail!(coded!(InvalidSide, "expected before or after")),
        }
    }
}

pub(crate) struct AcquiredSource {
    pub bytes: Vec<u8>,
    pub path: String,
    pub revision: ContentRevision,
    pub span: ByteSpan,
    pub verified: bool,
    pub handle: String,
    pub side: Option<SourceSide>,
    pub historical: Option<HistoricalSource>,
    /// For a `sym:` read: how the shown definition was chosen among its namesakes.
    pub definitions: Option<SymbolSelection>,
    /// `unpublished` when a `sym:` read was re-extracted from bytes newer than
    /// the published index.
    pub freshness: Option<&'static str>,
}

/// Acquires `target`; a `sym:` read ranks namesakes nearest to `origin` first.
pub(crate) fn acquire(
    store: &Store,
    target: &str,
    side: Option<SourceSide>,
    origin: &InvocationDirectory,
) -> Result<AcquiredSource> {
    if let Some(cursor) = target.strip_prefix("read:") {
        let (identity, offset) = cursor
            .rsplit_once('@')
            .with_coded_context(|| coded!(InvalidCursor, "expected read:HANDLE@BYTE_OFFSET"))?;
        if identity.starts_with("path:") {
            ensure!(
                side.is_none(),
                coded!(
                    InvalidSide,
                    "current path continuations have no historical side"
                )
            );
            let offset = offset
                .parse::<usize>()
                .with_coded_context(|| coded!(InvalidCursor, "invalid byte offset"))?;
            return path_read::acquire_path_continuation(store, identity, offset);
        }
        let (handle, recorded_side) = match identity.rsplit_once(':') {
            Some((handle, value @ ("before" | "after"))) => {
                (handle, Some(SourceSide::parse(value)?))
            }
            _ => (identity, None),
        };
        ensure!(
            side.is_none() || side == recorded_side,
            coded!(InvalidSide, "continuation side cannot change")
        );
        let mut source = acquire_handle(store, handle, recorded_side)?;
        let offset = offset
            .parse::<usize>()
            .with_coded_context(|| coded!(InvalidCursor, "invalid byte offset"))?;
        ensure!(
            offset >= source.span.start && offset <= source.span.end,
            coded!(InvalidCursor, "offset outside original source range")
        );
        source.span.start = offset;
        return Ok(source);
    }
    if is_result_handle_target(target) {
        return acquire_handle(store, target, side);
    }
    ensure!(
        side.is_none(),
        coded!(InvalidSide, "side requires a historical change handle")
    );
    if target.starts_with("sym:") {
        return symbol::acquire_symbol(store, target, origin);
    }
    path_read::acquire_path(store, target, origin)
}

/// Whether `target` names a current file read eligible for a local fallback.
pub(crate) fn is_current_path_target(target: &str) -> bool {
    if target.starts_with("sym:") {
        return false;
    }
    if target.starts_with("read:") {
        return target.starts_with("read:path:");
    }
    !is_result_handle_target(target)
}

/// Acquires current path bytes without using the optional result-set cache.
pub(crate) fn acquire_current_path_without_cache(
    store: &Store,
    target: &str,
    side: Option<SourceSide>,
    origin: &InvocationDirectory,
) -> Result<AcquiredSource> {
    ensure!(
        is_current_path_target(target),
        coded!(
            InvalidTarget,
            "cache-independent reads require a current path target"
        )
    );
    ensure!(
        side.is_none(),
        coded!(InvalidSide, "current path reads have no historical side")
    );
    if target.starts_with("read:path:") {
        return acquire(store, target, side, origin);
    }
    path_read::acquire_path_without_result_storage(store, target, origin)
}

fn is_result_handle_target(target: &str) -> bool {
    target.parse::<ResultHandle>().is_ok()
        || target
            .rsplit_once(':')
            .is_some_and(|(id, _)| uuid::Uuid::parse_str(id).is_ok())
}

fn acquire_handle(store: &Store, handle: &str, side: Option<SourceSide>) -> Result<AcquiredSource> {
    let (_, entry) = results::entry(store, handle)?;
    acquire_entry(store, handle, entry, side)
}

pub(crate) fn acquire_entry(
    store: &Store,
    handle: &str,
    entry: ResultEntry,
    side: Option<SourceSide>,
) -> Result<AcquiredSource> {
    match entry {
        ResultEntry::LiveSource(hit) => {
            ensure!(
                side.is_none(),
                coded!(InvalidSide, "live sources have no historical side")
            );
            let expected =
                ContentRevision::parse(hit.revision.as_deref().with_coded_context(|| {
                    coded!(SourceExcluded, "no indexed source revision")
                })?)?;
            let bytes = read_contained(
                &store.root,
                &tp_core::path_codec::decode_path(&hit.path)?,
                MAX_READ_BYTES,
            )?;
            let revision = ContentRevision::of(&bytes);
            ensure!(
                revision == expected,
                coded!(
                    StaleSource,
                    "source revision changed; search again or use explicit current path coordinates"
                )
            );
            let span = if hit.kind == "file" {
                ByteSpan::new(0, bytes.len())?
            } else {
                ByteSpan::new(hit.start, hit.end)?
            }
            .validate(bytes.len())?;
            Ok(AcquiredSource {
                bytes,
                path: hit.path,
                revision,
                span,
                verified: true,
                handle: handle.into(),
                side: None,
                historical: None,
                definitions: None,
                freshness: None,
            })
        }
        ResultEntry::Change(change) => {
            let side = side.with_coded_context(|| {
                coded!(SideRequired, "show a change with --side before or after")
            })?;
            let identity = match side {
                SourceSide::Before => change.before,
                SourceSide::After => change.after,
            }
            .with_coded_context(|| {
                coded!(
                    SourceUnavailable,
                    "change has no source on the requested side"
                )
            })?;
            acquire_historical(store, handle, side, identity)
        }
        ResultEntry::Commit(_) => {
            bail!(coded!(
                SourceUnavailable,
                "commit entries contain metadata; select a change source"
            ))
        }
    }
}

fn acquire_historical(
    store: &Store,
    handle: &str,
    side: SourceSide,
    identity: HistoricalSource,
) -> Result<AcquiredSource> {
    let repository = tp_core::git_runner::GitRepository::discover(&store.root)?;
    ensure!(
        repository.common_dir == tp_core::path_codec::decode_path(&identity.repository)?,
        coded!(
            ScopeBoundary,
            "historical source belongs to another repository"
        )
    );
    let decoded = tp_core::path_codec::decode_path(&identity.path)?;
    ensure!(
        !decoded.as_os_str().is_empty()
            && decoded
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        coded!(ScopeBoundary, "historical path is outside configured root")
    );
    let tree = repository.tree_entry(&identity.commit, &decoded)?;
    let header = tree.split(|b| *b == b'\t').next().unwrap_or_default();
    let header = std::str::from_utf8(header)?;
    let mut fields = header.split_whitespace();
    let mode = fields.next();
    ensure!(
        matches!(mode, Some("100644" | "100755")),
        coded!(
            SourceUnavailable,
            "historical path missing or not a regular file"
        )
    );
    ensure!(
        fields.next() == Some("blob") && fields.next() == Some(identity.blob.as_str()),
        coded!(
            SourceUnavailable,
            "historical path does not identify recorded blob"
        )
    );
    let bytes = repository
        .blob(&identity.blob)
        .with_coded_context(|| coded!(SourceUnavailable, "historical blob is unavailable"))?;
    let revision = ContentRevision::of(&bytes);
    ensure!(
        revision == identity.revision,
        coded!(
            SourceUnavailable,
            "historical content revision does not match recorded identity"
        )
    );
    let span = identity.span.validate(bytes.len())?;
    Ok(AcquiredSource {
        bytes,
        path: identity.path.clone(),
        revision,
        span,
        verified: true,
        handle: handle.into(),
        side: Some(side),
        historical: Some(identity),
        definitions: None,
        freshness: None,
    })
}
