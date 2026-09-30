//! `sym:` reads whose file changed after publication: wait briefly for a
//! serving daemon to republish, then locate the definition in current bytes.
use super::{AcquiredSource, acquire_entry};
use crate::{
    results::{self, Hit, ResultEntry},
    store::Store,
};
use anyhow::{Result, bail};
use rusqlite::OptionalExtension;
use std::time::{Duration, Instant};

/// Longest a stale `sym:` read waits for a serving daemon to republish.
const REPUBLISH_WAIT: Duration = Duration::from_secs(3);
const REPUBLISH_POLL: Duration = Duration::from_millis(100);

/// A `sym:` read of `entry`. When its file changed since publication, a serving
/// daemon gets [`REPUBLISH_WAIT`] to republish; the definition is then located
/// in current bytes, verified only if the index now publishes those bytes and
/// otherwise returned unverified as `freshness: unpublished`.
pub(super) fn acquire_symbol_entry(
    store: &Store,
    handle: &str,
    entry: ResultEntry,
) -> Result<AcquiredSource> {
    let ResultEntry::LiveSource(hit) = &entry else {
        return acquire_entry(store, handle, entry, None);
    };
    let hit = hit.clone();
    let seen = store.generation()?;
    let error = match acquire_entry(store, handle, entry, None) {
        Err(error) if is_stale_source(&error) => error,
        read => return read,
    };
    let republished = await_republish(store, seen);
    // Binary or unextractable current bytes leave the stale answer standing.
    let mut source = match acquire_reextracted(store, &hit) {
        Ok(Some(source)) => source,
        Ok(None) => return Err(error),
        Err(unavailable) => bail!("{error}; current bytes: {unavailable:#}"),
    };
    if republished && published_revision(store, &hit.path)? == Some(source.revision.to_string()) {
        source.verified = true;
        source.freshness = None;
    }
    Ok(source)
}

/// The revision the current publication records for `path`.
fn published_revision(store: &Store, path: &str) -> Result<Option<String>> {
    Ok(store
        .conn
        .query_row("SELECT revision FROM files WHERE path=?1", [path], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

fn is_stale_source(error: &anyhow::Error) -> bool {
    error.to_string().starts_with("stale_source:")
}

/// Waits while a daemon serves this index for a generation after `seen`. A
/// worktree read through a parent's index is never fixed by a republish.
fn await_republish(store: &Store, seen: i64) -> bool {
    if store.root != store.index_root() || !crate::daemon::running(store.index_cache()) {
        return false;
    }
    let started = Instant::now();
    while started.elapsed() < REPUBLISH_WAIT {
        std::thread::sleep(REPUBLISH_POLL);
        if store.generation().is_ok_and(|generation| generation > seen) {
            return true;
        }
    }
    false
}

/// `hit`'s definition in its original container, located in the file's current
/// bytes and saved as its own result so continuations address those bytes.
fn acquire_reextracted(store: &Store, hit: &Hit) -> Result<Option<AcquiredSource>> {
    let Some(definition) = crate::source::reextract_definition(&store.root, hit)? else {
        return Ok(None);
    };
    let set = results::save_entries(
        store,
        store.generation()?,
        serde_json::json!({}),
        vec![ResultEntry::LiveSource(definition.hit())],
        false,
    )?;
    Ok(Some(AcquiredSource {
        path: definition.path,
        revision: definition.revision,
        span: definition.span,
        bytes: definition.bytes,
        verified: false,
        handle: format!("{set}:1"),
        side: None,
        historical: None,
        definitions: None,
        freshness: Some("unpublished"),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::acquire;
    use crate::search::InvocationDirectory;
    use crate::source::line_span;
    use crate::{output::OutputBudget, store::Store};

    #[test]
    fn sym_read_falls_back_to_current_bytes() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lib.rs"), "fn target() {}\n").unwrap();
        let mut store = Store::open(root.path(), cache.path()).unwrap();
        store.index().unwrap();
        let published = acquire(&store, "sym:target", None, &InvocationDirectory::root()).unwrap();
        assert!(published.verified);
        std::fs::write(
            root.path().join("lib.rs"),
            "// moved down\n\nfn target() -> u8 { 1 }\n",
        )
        .unwrap();

        let current = acquire(&store, "sym:target", None, &InvocationDirectory::root()).unwrap();
        assert!(!current.verified);
        assert_eq!(current.freshness, Some("unpublished"));
        assert_eq!(
            &current.bytes[current.span.start..current.span.end],
            b"fn target() -> u8 { 1 }"
        );
        assert_eq!(
            current
                .definitions
                .as_ref()
                .map(|selection| selection.declarations),
            Some(1)
        );
        let rendered: serde_json::Value = serde_json::from_str(
            &crate::source::show(&store, "sym:target", &OutputBudget::new(4_000).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(rendered["freshness"], "unpublished");
        assert_eq!(rendered["verified"], false);
        assert_eq!(rendered["lines"][0]["line"], 3);
        // Handles stay immutable: the published read's handle is stale now.
        let error = acquire(
            &store,
            &published.handle,
            None,
            &InvocationDirectory::root(),
        )
        .err()
        .unwrap();
        assert!(error.to_string().starts_with("stale_source:"), "{error:#}");

        std::fs::write(root.path().join("lib.rs"), b"fn target() {}\0").unwrap();
        let binary = acquire(&store, "sym:target", None, &InvocationDirectory::root())
            .err()
            .unwrap();
        let message = format!("{binary:#}");
        assert!(message.starts_with("stale_source:"), "{message}");
        assert!(message.contains("source_excluded"), "{message}");

        std::fs::write(
            root.path().join("lib.rs"),
            "// moved down\n\nfn target() -> u8 { 1 }\n",
        )
        .unwrap();
        store.index().unwrap();
        let republished =
            acquire(&store, "sym:target", None, &InvocationDirectory::root()).unwrap();
        assert!(republished.verified);
        assert_eq!(republished.freshness, None);
    }

    #[test]
    fn stale_qualified_symbol_stays_in_the_selected_method_container() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("lib.rs"),
            "struct A;\nimpl A {\n    fn open() { 10 }\n}\nstruct B;\nimpl B {\n    fn open() { 20 }\n}\n",
        )
        .unwrap();
        let mut store = Store::open(root.path(), cache.path()).unwrap();
        store.index().unwrap();
        let original = acquire(&store, "sym:A::open", None, &InvocationDirectory::root()).unwrap();
        assert!(original.verified);
        assert_eq!(
            &original.bytes[original.span.start..original.span.end],
            b"fn open() { 10 }"
        );

        // Put B's same-named method at A's old line; nearest-name lookup alone
        // would return this body for the still-qualified `sym:A::open` query.
        std::fs::write(
            root.path().join("lib.rs"),
            "struct B;\nimpl B {\n    fn open() { 99 }\n}\nstruct A;\nimpl A {\n    fn open() { 11 }\n}\n",
        )
        .unwrap();
        let current = acquire(&store, "sym:A::open", None, &InvocationDirectory::root()).unwrap();
        assert!(!current.verified);
        assert_eq!(current.freshness, Some("unpublished"));
        let body = &current.bytes[current.span.start..current.span.end];
        assert_eq!(body, b"fn open() { 11 }");
        assert!(!body.windows(2).any(|window| window == b"99"));
        assert_eq!(
            line_span(&current.bytes, current.span.start, current.span.end).0,
            7
        );
    }
}
