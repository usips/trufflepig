//! Publishes a staged index into the live index in one transaction
//! ([publication storage](../../docs/index-storage-contract.md#publication-storage)).
//! An empty live index receives every staged row; otherwise only the rows of
//! added, removed, and differing files change.
mod definition_matching;
mod diff_writes;
mod fact_digest;
mod observation_journal;
mod publication_diff;

pub(super) use observation_journal::{create_journal, observations_since};

use super::{Coverage, Publication, regions};
use anyhow::Result;
use rusqlite::{Connection, params};
use std::path::Path;

/// FTS5 pages a merge may write per changed document, and its floor; the
/// negative `merge` argument lets it fold even two segments together.
const MERGE_PAGES_PER_DOCUMENT: usize = 8;
const MIN_MERGE_PAGES: usize = 64;

pub(super) fn publish(
    conn: &mut Connection,
    staged: &Path,
    coverage: &Coverage,
    fingerprint: &str,
    publication: &Publication,
) -> Result<()> {
    conn.execute(
        "ATTACH DATABASE ?1 AS staged",
        [staged
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 cache directory"))?],
    )?;
    // The diff deletes children before parents; per-row foreign key probes of
    // unindexed relationship columns would make each deletion scan the table.
    let result = conn
        .execute_batch("PRAGMA foreign_keys=OFF;")
        .map_err(Into::into)
        .and_then(|()| publish_attached(conn, coverage, fingerprint, publication));
    let restored = conn.execute_batch("PRAGMA foreign_keys=ON;");
    let detach = conn.execute_batch("DETACH DATABASE staged");
    result?;
    restored?;
    detach?;
    Ok(())
}

fn publish_attached(
    conn: &mut Connection,
    coverage: &Coverage,
    fingerprint: &str,
    publication: &Publication,
) -> Result<()> {
    let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    observation_journal::append_observations(&transaction, publication)?;
    let live_files: i64 =
        transaction.query_row("SELECT count(*) FROM main.files", [], |row| row.get(0))?;
    if live_files == 0 {
        replace_all(&transaction)?;
    } else {
        let diff = publication_diff::plan(&transaction)?;
        let churn = diff_writes::apply(&transaction, &diff)?;
        compact_documents(&transaction, churn.changed)?;
    }
    transaction
        .execute_batch("UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='generation';")?;
    transaction.execute(
        "UPDATE meta SET value=?1 WHERE key='coverage'",
        [serde_json::to_string(coverage)?],
    )?;
    set_meta(&transaction, "fingerprint", fingerprint)?;
    set_meta(&transaction, "regions_contract", &regions::contract())?;
    let mut publication = publication.clone();
    publication.capture_completed_ms = timestamp_ms()?.max(publication.capture_started_ms);
    set_meta(
        &transaction,
        "publication",
        &serde_json::to_string(&publication)?,
    )?;
    transaction.commit()?;
    Ok(())
}

/// Copies every staged row, ids included, into an index that holds no files.
fn replace_all(tx: &Connection) -> Result<()> {
    tx.execute_batch(
        "DELETE FROM relationships; DELETE FROM occurrences; DELETE FROM definitions;
         DELETE FROM documents; DELETE FROM regions; DELETE FROM contents; DELETE FROM extraction_cache;
         INSERT INTO contents(revision,bytes) SELECT revision,bytes FROM staged.contents;
         INSERT INTO files(id,path,revision,language,status,bytes,stat_size,stat_mtime_ns,stat_ctime_ns,stat_inode,stat_device)
             SELECT id,path,revision,language,status,bytes,stat_size,stat_mtime_ns,stat_ctime_ns,stat_inode,stat_device FROM staged.files;
         INSERT INTO definitions(id,file_id,name,kind,start,end,container)
             SELECT id,file_id,name,kind,start,end,container FROM staged.definitions;
         INSERT INTO occurrences(id,file_id,name,start,end,role,target,candidates,provenance)
             SELECT id,file_id,name,start,end,role,target,candidates,provenance FROM staged.occurrences;
         INSERT INTO relationships(source,target,kind,file_id,start,end,provenance)
             SELECT source,target,kind,file_id,start,end,provenance FROM staged.relationships;
         INSERT INTO regions(id,file_id,start,end,name,kind,body)
             SELECT id,file_id,start,end,name,kind,body FROM staged.regions;
         INSERT INTO documents(rowid,name,expanded,body,path)
             SELECT rowid,name,expanded,body,path FROM staged.documents;
         INSERT INTO extraction_cache(revision,grammar,version,facts)
             SELECT revision,grammar,version,facts FROM staged.extraction_cache;
         DELETE FROM contents WHERE revision NOT IN (SELECT revision FROM files WHERE revision IS NOT NULL);",
    )?;
    Ok(())
}

/// Bounds FTS5 delete markers: a merge sized by the churn, or a full optimize
/// once a quarter or more of the documents changed.
fn compact_documents(tx: &Connection, changed: usize) -> Result<()> {
    if changed == 0 {
        return Ok(());
    }
    let total: i64 = tx.query_row("SELECT count(*) FROM main.regions", [], |row| row.get(0))?;
    if changed.saturating_mul(4) >= usize::try_from(total)?.max(1) {
        tx.execute_batch("INSERT INTO main.documents(documents) VALUES('optimize');")?;
    } else {
        let pages = changed
            .saturating_mul(MERGE_PAGES_PER_DOCUMENT)
            .max(MIN_MERGE_PAGES);
        tx.execute(
            "INSERT INTO main.documents(documents,rank) VALUES('merge',?1)",
            [-i64::try_from(pages)?],
        )?;
    }
    Ok(())
}

fn set_meta(tx: &Connection, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub(super) fn timestamp_ms() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}
