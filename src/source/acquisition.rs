//! Acquire and verify one immutable identity before rendering its remaining byte range.
use super::{MAX_READ_BYTES, current_span, read_contained};
use crate::{
    identity::{ByteSpan, ContentRevision, ResultHandle},
    results::{self, HistoricalSource, Hit, ResultEntry},
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use std::path::Component;

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
            _ => bail!("invalid_side: expected before or after"),
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
}

pub(crate) fn acquire(
    store: &Store,
    target: &str,
    side: Option<SourceSide>,
) -> Result<AcquiredSource> {
    if let Some(cursor) = target.strip_prefix("read:") {
        let (identity, offset) = cursor
            .rsplit_once('@')
            .context("invalid_cursor: expected read:HANDLE@BYTE_OFFSET")?;
        let (handle, recorded_side) = match identity.rsplit_once(':') {
            Some((handle, value @ ("before" | "after"))) => {
                (handle, Some(SourceSide::parse(value)?))
            }
            _ => (identity, None),
        };
        ensure!(
            side.is_none() || side == recorded_side,
            "invalid_side: continuation side cannot change"
        );
        let mut source = acquire_handle(store, handle, recorded_side)?;
        let offset = offset
            .parse::<usize>()
            .context("invalid_cursor: invalid byte offset")?;
        ensure!(
            offset >= source.span.start && offset <= source.span.end,
            "invalid_cursor: offset outside original source range"
        );
        source.span.start = offset;
        return Ok(source);
    }
    if target.parse::<ResultHandle>().is_ok()
        || target
            .rsplit_once(':')
            .is_some_and(|(id, _)| uuid::Uuid::parse_str(id).is_ok())
    {
        return acquire_handle(store, target, side);
    }
    ensure!(
        side.is_none(),
        "invalid_side: side requires a historical change handle"
    );
    acquire_path(store, target)
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
                "invalid_side: live sources have no historical side"
            );
            let expected = ContentRevision::parse(
                hit.revision
                    .as_deref()
                    .context("source_excluded: no indexed source revision")?,
            )?;
            let bytes = read_contained(
                &store.root,
                &crate::store::decode_path(&hit.path)?,
                MAX_READ_BYTES,
            )?;
            let revision = ContentRevision::of(&bytes);
            ensure!(
                revision == expected,
                "stale_source: source revision changed; search again or use explicit current path coordinates"
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
            })
        }
        ResultEntry::Change(change) => {
            let side = side.context("side_required: show a change with --side before or after")?;
            let identity = match side {
                SourceSide::Before => change.before,
                SourceSide::After => change.after,
            }
            .context("source_unavailable: change has no source on the requested side")?;
            acquire_historical(store, handle, side, identity)
        }
        ResultEntry::Commit(_) => {
            bail!("source_unavailable: commit entries contain metadata; select a change source")
        }
    }
}

fn acquire_historical(
    store: &Store,
    handle: &str,
    side: SourceSide,
    identity: HistoricalSource,
) -> Result<AcquiredSource> {
    let repository = crate::history::git::GitRepository::discover(&store.root)?;
    ensure!(
        repository.common_dir == crate::store::decode_path(&identity.repository)?,
        "scope_boundary: historical source belongs to another repository"
    );
    let decoded = crate::store::decode_path(&identity.path)?;
    ensure!(
        !decoded.as_os_str().is_empty()
            && decoded
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "scope_boundary: historical path is outside configured root"
    );
    let tree = repository.tree_entry(&identity.commit, &decoded)?;
    let header = tree.split(|b| *b == b'\t').next().unwrap_or_default();
    let header = std::str::from_utf8(header)?;
    let mut fields = header.split_whitespace();
    let mode = fields.next();
    ensure!(
        matches!(mode, Some("100644" | "100755")),
        "source_unavailable: historical path missing or not a regular file"
    );
    ensure!(
        fields.next() == Some("blob") && fields.next() == Some(identity.blob.as_str()),
        "source_unavailable: historical path does not identify recorded blob"
    );
    let bytes = repository
        .blob(&identity.blob)
        .context("source_unavailable: historical blob is unavailable")?;
    let revision = ContentRevision::of(&bytes);
    ensure!(
        revision == identity.revision,
        "source_unavailable: historical content revision does not match recorded identity"
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
    })
}

fn acquire_path(store: &Store, target: &str) -> Result<AcquiredSource> {
    let target = target.strip_prefix("path:").unwrap_or(target);
    let (path, lines) = match target.rsplit_once(':') {
        Some((path, range)) if range.contains('-') => {
            let (first, last) = range.split_once('-').expect("range separator");
            (path, (first.parse::<usize>()?, last.parse::<usize>()?))
        }
        _ => (target, (1, usize::MAX)),
    };
    let bytes = read_contained(
        &store.root,
        &crate::store::decode_path(path)?,
        MAX_READ_BYTES,
    )?;
    let revision = ContentRevision::of(&bytes);
    let (start, end) = current_span(&bytes, lines.0, lines.1)?;
    let (start_line, end_line) = super::line_span(&bytes, start, end);
    let hit = Hit {
        handle: String::new(),
        path: path.into(),
        revision: Some(revision.to_string()),
        start,
        end,
        start_line,
        end_line,
        name: path.into(),
        kind: "source_read".into(),
        container: None,
        provenance: Some("explicit_path_read".into()),
        resolution: None,
        candidates: Vec::new(),
        target: None,
        snippet: None,
    };
    let set = results::save_entries(
        store,
        store.generation()?,
        serde_json::json!({}),
        vec![ResultEntry::LiveSource(hit)],
        false,
    )?;
    Ok(AcquiredSource {
        bytes,
        path: path.into(),
        revision,
        span: ByteSpan::new(start, end)?,
        verified: false,
        handle: format!("{set}:1"),
        side: None,
        historical: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputBudget;

    #[test]
    fn owned_read_survives_member_result_eviction_and_keeps_provenance() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let bytes = "// immutable source line\n".repeat(220);
        std::fs::write(root.path().join("lib.rs"), &bytes).unwrap();
        let store = Store::open(root.path(), cache.path()).unwrap();
        let original = acquire(&store, "path:lib.rs", None).unwrap();
        let (_, entry) = results::entry(&store, &original.handle).unwrap();
        store.conn.execute("DELETE FROM result_sets", []).unwrap();

        let handle = format!("{}:1", uuid::Uuid::new_v4());
        let source = acquire_entry(&store, &handle, entry.clone(), None).unwrap();
        let metadata =
            serde_json::json!({"member":"engine","members":[{"name":"engine","root":root.path()}]});
        let budget = OutputBudget::new(600).unwrap();
        let output = crate::source::render_owned(source, &budget, &metadata).unwrap();
        assert!(budget.fits(&output));
        let response: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(response["member"], metadata["member"]);
        assert_eq!(response["members"], metadata["members"]);
        assert!(
            response["next"]
                .as_str()
                .unwrap()
                .starts_with(&format!("read:{handle}@"))
        );

        let source = acquire_entry(&store, &handle, entry.clone(), None).unwrap();
        let oversized = serde_json::json!({"member":"engine ".repeat(1000)});
        assert!(
            crate::source::render_owned(source, &budget, &oversized)
                .unwrap_err()
                .to_string()
                .contains("budget_too_small")
        );

        std::fs::write(root.path().join("lib.rs"), "// replacement\n").unwrap();
        assert!(
            acquire_entry(&store, &handle, entry, None)
                .err()
                .unwrap()
                .to_string()
                .contains("stale_source")
        );
    }

    #[test]
    fn owned_source_metadata_cannot_replace_verified_identity() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lib.rs"), "fn original() {}\n").unwrap();
        let store = Store::open(root.path(), cache.path()).unwrap();
        let source = acquire(&store, "path:lib.rs", None).unwrap();
        let error = crate::source::render_owned(
            source,
            &OutputBudget::new(600).unwrap(),
            &serde_json::json!({"path":"other.rs"}),
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid_metadata"));
    }
}
