//! Populate an unpublished in-memory store with one current source file.
use super::{Store, regions, scan};
use crate::{extract::Extraction, identity::ContentRevision};
use anyhow::Result;
use rusqlite::params;

/// Inserts the search facts needed to query `bytes` without touching a published index.
pub(crate) fn insert(
    store: &Store,
    path: &str,
    bytes: &[u8],
    extraction: &Extraction,
) -> Result<()> {
    let revision = ContentRevision::of(bytes).to_string();
    let tx = store.conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO contents(revision,bytes) VALUES(?1,?2)",
        params![revision, bytes],
    )?;
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
    let file_id = tx.last_insert_rowid();
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
