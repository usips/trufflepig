//! Current-file reads use result handles when available and stateless cursors otherwise.

use super::super::current_span;
use super::{AcquiredSource, MAX_READ_BYTES, read_contained};
use crate::{
    results::{self, Hit, ResultEntry},
    search::InvocationDirectory,
    store::Store,
};
use anyhow::{Result, ensure};
use std::{
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
};
use tp_core::coded_error::CodedContext;
use tp_core::{
    coded_error::{ErrorCode, coded_error},
    identity::{ByteSpan, ContentRevision},
};

mod stateless_path_read;
#[cfg(test)]
mod tests;
pub(crate) use stateless_path_read::{cursor_matches_root, stateless_hit_handle};

#[derive(Clone, Copy)]
pub(super) enum PathResultStorage {
    BestEffort,
    Disabled,
}

pub(super) fn acquire_path(
    store: &Store,
    target: &str,
    origin: &InvocationDirectory,
) -> Result<AcquiredSource> {
    acquire_path_with_storage(store, target, origin, PathResultStorage::BestEffort)
}

pub(super) fn acquire_path_without_result_storage(
    store: &Store,
    target: &str,
    origin: &InvocationDirectory,
) -> Result<AcquiredSource> {
    acquire_path_with_storage(store, target, origin, PathResultStorage::Disabled)
}

fn acquire_path_with_storage(
    store: &Store,
    target: &str,
    origin: &InvocationDirectory,
    result_storage: PathResultStorage,
) -> Result<AcquiredSource> {
    let target = target.strip_prefix("path:").unwrap_or(target);
    let (path, lines, range) = match target.rsplit_once(':') {
        Some((path, range)) if range.contains('-') => {
            let (first, last) = range.split_once('-').expect("range separator");
            (
                path,
                (first.parse::<usize>()?, last.parse::<usize>()?),
                Some(range),
            )
        }
        _ => (target, (1, usize::MAX), None),
    };
    let relative = tp_core::path_codec::decode_path(path)?;
    let encoded_path = tp_core::path_codec::encode_path(&relative);
    let bytes = match read_contained(&store.root, &relative, MAX_READ_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if ErrorCode::SourceUnavailable.is_code_of(&error) => {
            let invocation_relative = origin.member_relative_path()?.join(&relative);
            if invocation_relative != relative
                && read_contained(&store.root, &invocation_relative, MAX_READ_BYTES).is_ok()
                && let Some(unavailable) = coded_error(&error)
            {
                let suggested = tp_core::path_codec::encode_path(&invocation_relative);
                let range = range.map_or_else(String::new, |range| format!(":{range}"));
                anyhow::bail!(unavailable.appended(&format!(
                    "; path values are relative to the selected member root; use `path:{suggested}{range}`"
                )));
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let revision = ContentRevision::of(&bytes);
    let revision_text = revision.to_string();
    let (start, end) = current_span(&bytes, lines.0, lines.1)?;
    let handle = match result_storage {
        PathResultStorage::Disabled => {
            stateless_handle(&store.root, &relative, start, end, &revision_text)
        }
        PathResultStorage::BestEffort => {
            let (start_line, end_line) = super::super::line_span(&bytes, start, end);
            let hit = Hit {
                handle: String::new(),
                path: encoded_path.clone(),
                revision: Some(revision_text.clone()),
                start,
                end,
                start_line,
                end_line,
                name: encoded_path.clone(),
                kind: "source_read".into(),
                container: None,
                provenance: Some("explicit_path_read".into()),
                resolution: None,
                candidates: Vec::new(),
                target: None,
                repeats: None,
                snippet: None,
                differs: false,
            };
            let generation = store.generation()?;
            match results::save_entries(
                store,
                generation,
                serde_json::json!({}),
                vec![ResultEntry::LiveSource(hit)],
                false,
            ) {
                Ok(set) => format!("{set}:1"),
                Err(error)
                    if !tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error)
                        && crate::store::is_index_cache_unavailable(&error) =>
                {
                    stateless_handle(&store.root, &relative, start, end, &revision_text)
                }
                Err(error) => return Err(error),
            }
        }
    };
    Ok(AcquiredSource {
        bytes,
        path: encoded_path,
        revision,
        span: ByteSpan::new(start, end)?,
        verified: false,
        handle,
        side: None,
        historical: None,
        definitions: None,
        freshness: None,
    })
}

pub(super) fn acquire_path_continuation(
    store: &Store,
    identity: &str,
    offset: usize,
) -> Result<AcquiredSource> {
    let encoded_identity = identity.strip_prefix("path:").with_coded_context(|| {
        coded!(
            InvalidCursor,
            "expected path:ROOT_DIGEST:ENCODED_PATH:START-END@BLAKE3"
        )
    })?;
    let (expected_root, encoded_identity) =
        encoded_identity.split_once(':').with_coded_context(|| {
            coded!(
                InvalidCursor,
                "expected path:ROOT_DIGEST:ENCODED_PATH:START-END@BLAKE3"
            )
        })?;
    let expected_root = PathCursorRootIdentity::parse(expected_root)?;
    ensure!(
        expected_root == PathCursorRootIdentity::for_root(&store.root),
        coded!(
            InvalidCursor,
            "cursor belongs to a different repository root"
        )
    );
    let (path_and_span, expected_revision) =
        encoded_identity.rsplit_once('@').with_coded_context(|| {
            coded!(
                InvalidCursor,
                "expected path:ROOT_DIGEST:ENCODED_PATH:START-END@BLAKE3"
            )
        })?;
    let (encoded_path, requested_span) =
        path_and_span.rsplit_once(':').with_coded_context(|| {
            coded!(
                InvalidCursor,
                "expected path:ROOT_DIGEST:ENCODED_PATH:START-END@BLAKE3"
            )
        })?;
    let (start, end) = requested_span
        .split_once('-')
        .with_coded_context(|| coded!(InvalidCursor, "expected original byte bounds"))?;
    let start = start
        .parse::<usize>()
        .with_coded_context(|| coded!(InvalidCursor, "invalid original start byte"))?;
    let end = end
        .parse::<usize>()
        .with_coded_context(|| coded!(InvalidCursor, "invalid original end byte"))?;
    let revision = ContentRevision::parse(expected_revision)
        .with_coded_context(|| coded!(InvalidCursor, "invalid source revision"))?;
    let relative = tp_core::path_codec::decode_path(encoded_path)
        .with_coded_context(|| coded!(InvalidCursor, "invalid encoded path"))?;
    ensure!(
        tp_core::path_codec::encode_path(&relative) == encoded_path,
        coded!(InvalidCursor, "path is not canonically encoded")
    );
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        coded!(InvalidCursor, "path is outside the configured root")
    );
    let bytes = read_contained(&store.root, &relative, MAX_READ_BYTES)?;
    ensure!(
        ContentRevision::of(&bytes) == revision,
        coded!(
            StaleSource,
            "current path changed; start a new explicit path read"
        )
    );
    let original_span = ByteSpan::new(start, end)
        .with_coded_context(|| coded!(InvalidCursor, "invalid original byte bounds"))?
        .validate(bytes.len())
        .with_coded_context(|| {
            coded!(InvalidCursor, "original byte bounds outside current source")
        })?;
    ensure!(
        is_line_boundary(&bytes, original_span.start)
            && is_line_boundary(&bytes, original_span.end),
        coded!(
            InvalidCursor,
            "original byte bounds must be whole-line boundaries"
        )
    );
    ensure!(
        offset >= original_span.start && offset <= original_span.end,
        coded!(InvalidCursor, "offset outside original source range")
    );
    ensure!(
        is_line_boundary(&bytes, offset),
        coded!(InvalidCursor, "offset must be a whole-line boundary")
    );
    Ok(AcquiredSource {
        bytes,
        path: tp_core::path_codec::encode_path(&relative),
        revision,
        span: ByteSpan::new(offset, original_span.end)?,
        verified: false,
        handle: identity.into(),
        side: None,
        historical: None,
        definitions: None,
        freshness: None,
    })
}

fn stateless_handle(
    root: &Path,
    relative: &Path,
    start: usize,
    end: usize,
    revision: &str,
) -> String {
    let root_identity = PathCursorRootIdentity::for_root(root);
    format!(
        "path:{root_identity}:{}:{start}-{end}@{revision}",
        tp_core::path_codec::encode_path(relative),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PathCursorRootIdentity(blake3::Hash);

impl PathCursorRootIdentity {
    /// Hashes the bytes of the canonical `Store::root` path.
    fn for_root(root: &Path) -> Self {
        Self(blake3::hash(root.as_os_str().as_bytes()))
    }

    fn parse(value: &str) -> Result<Self> {
        let identity = blake3::Hash::from_hex(value)
            .with_coded_context(|| coded!(InvalidCursor, "invalid canonical root digest"))?;
        ensure!(
            identity.to_hex().as_str() == value,
            coded!(InvalidCursor, "root digest is not canonically encoded")
        );
        Ok(Self(identity))
    }
}

impl std::fmt::Display for PathCursorRootIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0.to_hex())
    }
}

fn is_line_boundary(bytes: &[u8], offset: usize) -> bool {
    offset == 0 || offset == bytes.len() || bytes.get(offset.saturating_sub(1)) == Some(&b'\n')
}
