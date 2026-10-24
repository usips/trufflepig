//! Populate an unpublished in-memory store with current source files.
use super::{Store, regions, scan};
use crate::{extract::Extraction, identity::ContentRevision};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};

/// Registers all paths in one in-memory transaction before the shared path filter binds.
pub(crate) fn register_paths<'a>(
    store: &Store,
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let tx = store.conn.unchecked_transaction()?;
    for path in paths {
        tx.execute(
            "INSERT OR IGNORE INTO files(path,revision,language,status,bytes) VALUES(?1,NULL,?2,'unread',0)",
            params![path, crate::extract::language(path)],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Records why a registered path has no searchable current-byte snapshot.
pub(crate) fn mark_unavailable(
    store: &Store,
    path: &str,
    status: &str,
    length: usize,
) -> Result<()> {
    let tx = store.conn.unchecked_transaction()?;
    let updated = tx.execute(
        "UPDATE files SET language=?2,status=?3,bytes=?4 WHERE path=?1 AND revision IS NULL",
        params![path, crate::extract::language(path), status, length as i64],
    )?;
    if updated == 0 {
        tx.execute(
            "INSERT INTO files(path,revision,language,status,bytes) VALUES(?1,NULL,?2,?3,?4)",
            params![path, crate::extract::language(path), status, length as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Inserts the search facts needed to query `bytes` without touching a published index.
/// Identical content may back more than one current path in the same in-memory store.
pub(crate) fn insert(
    store: &Store,
    path: &str,
    bytes: &[u8],
    extraction: &Extraction,
) -> Result<()> {
    let revision = ContentRevision::of(bytes).to_string();
    let tx = store.conn.unchecked_transaction()?;
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO contents(revision,bytes) VALUES(?1,?2)",
        params![revision, bytes],
    )?;
    if inserted == 0 {
        let stored: Vec<u8> = tx
            .query_row(
                "SELECT bytes FROM contents WHERE revision=?1",
                [&revision],
                |row| row.get(0),
            )
            .context("source_revision_collision: current content row disappeared")?;
        ensure!(
            stored.as_slice() == bytes,
            "source_revision_collision: one revision names different current bytes"
        );
    }
    let existing: Option<(i64, Option<String>)> = tx
        .query_row(
            "SELECT id,revision FROM files WHERE path=?1",
            [path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let file_id = match existing {
        Some((id, None)) => {
            tx.execute(
                "UPDATE files SET revision=?2,language=?3,status=?4,bytes=?5 WHERE id=?1",
                params![
                    id,
                    revision,
                    extraction.language,
                    extraction.status,
                    bytes.len() as i64,
                ],
            )?;
            id
        }
        Some((_, Some(_))) => {
            anyhow::bail!("current_source_duplicate: path already has a source snapshot")
        }
        None => {
            tx.execute(
                "INSERT INTO files(path,revision,language,status,bytes) VALUES(?1,?2,?3,?4,?5)",
                params![
                    path,
                    revision,
                    extraction.language,
                    extraction.status,
                    bytes.len() as i64,
                ],
            )?;
            tx.last_insert_rowid()
        }
    };
    scan::insert_facts(&tx, file_id, extraction)?;
    if extraction.language != "text" {
        tx.execute(
            "INSERT INTO definitions(file_id,name,kind,start,end,container) VALUES(?1,?2,'module',0,?3,NULL)",
            params![file_id, path, bytes.len() as i64],
        )?;
    }
    regions::insert(&tx, file_id, path, bytes, &extraction.definitions)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{search::PathFilter, store::Store};

    #[test]
    fn registered_paths_and_shared_content_stay_in_memory() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::create_dir_all(root.join("nested/lib")).unwrap();
        let cache = root.join("selected-cache");
        let store = Store::unpublished(&root, &cache).unwrap();
        let bytes = b"pub fn shared_source() {}\n";
        let extraction = crate::extract::extract("lib/a.rs", bytes);

        register_paths(&store, ["lib/a.rs", "nested/lib/b.rs"]).unwrap();
        mark_unavailable(&store, "nested/lib/blocked.rs", "source_unavailable", 0).unwrap();
        insert(&store, "lib/a.rs", bytes, &extraction).unwrap();
        insert(&store, "nested/lib/b.rs", bytes, &extraction).unwrap();

        let files: i64 = store
            .conn
            .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
            .unwrap();
        let contents: i64 = store
            .conn
            .query_row("SELECT count(*) FROM contents", [], |row| row.get(0))
            .unwrap();
        assert_eq!(files, 3);
        assert_eq!(contents, 1);

        let bound = PathFilter::prefix("lib/").bind(&store.conn).unwrap();
        assert!(bound.matches("lib/a.rs"));
        assert!(!bound.matches("nested/lib/b.rs"));
        assert_eq!(store.coverage().unwrap().total_files, 0);
        assert!(!cache.exists());
    }

    #[test]
    fn duplicate_revision_with_different_bytes_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::unpublished(&root, &directory.path().join("cache")).unwrap();
        let bytes = b"pub fn expected() {}\n";
        let revision = ContentRevision::of(bytes).to_string();
        store
            .conn
            .execute(
                "INSERT INTO contents(revision,bytes) VALUES(?1,?2)",
                params![revision, b"different bytes"],
            )
            .unwrap();

        let error = insert(
            &store,
            "lib.rs",
            bytes,
            &crate::extract::extract("lib.rs", bytes),
        )
        .unwrap_err();
        assert!(error.to_string().starts_with("source_revision_collision:"));
    }
}
