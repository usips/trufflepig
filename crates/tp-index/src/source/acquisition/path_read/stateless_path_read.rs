use super::{PathCursorRootIdentity, stateless_handle};
use anyhow::{Context, Result, ensure};
use std::path::{Component, Path};
use tp_core::coded_error::CodedContext;
use tp_core::identity::{ByteSpan, ContentRevision};

/// Builds a process-independent `show` handle for a current hit.
///
/// The cursor spans complete source lines so the existing continuation reader
/// can validate it without result-cache storage.
pub(crate) fn stateless_hit_handle(
    root: &Path,
    relative: &Path,
    bytes: &[u8],
    start: usize,
    end: usize,
) -> Result<String> {
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        coded!(InvalidPath, "hit path is outside the configured root")
    );
    let span = ByteSpan::new(start, end)
        .with_coded_context(|| coded!(InvalidRange, "invalid current-source hit span"))?
        .validate(bytes.len())
        .with_coded_context(|| coded!(InvalidRange, "current-source hit span exceeds file"))?;
    let (first, last) = super::super::super::line_span(bytes, span.start, span.end);
    let (line_start, line_end) = super::super::super::current_span(bytes, first, last)?;
    let revision = ContentRevision::of(bytes).to_string();
    let identity = stateless_handle(root, relative, line_start, line_end, &revision);
    Ok(format!("read:{identity}@{line_start}"))
}

/// Whether an opaque current-path cursor is bound to `root`.
pub(crate) fn cursor_matches_root(target: &str, root: &Path) -> Result<bool> {
    let Some(cursor) = target.strip_prefix("read:path:") else {
        return Ok(false);
    };
    let (identity, _) = cursor
        .split_once(':')
        .with_coded_context(|| coded!(InvalidCursor, "expected path:ROOT_DIGEST:ENCODED_PATH"))?;
    let identity = PathCursorRootIdentity::parse(identity)?;
    let root = root
        .canonicalize()
        .context("canonicalize repository root")?;
    Ok(identity == PathCursorRootIdentity::for_root(&root))
}
