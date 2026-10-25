//! Applies a [`PublicationDiff`] inside the publication transaction: rows of
//! removed files and of each differing (file, table) pair are deleted by
//! `file_id` and reinserted from the staged index in live id space.
use super::fact_digest::translate_candidates;
use super::publication_diff::PublicationDiff;
use crate::store::scan::{extraction_cache_version, grammar};
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashSet;

/// Lexical documents deleted and inserted by one publication.
pub(super) struct DocumentChurn {
    pub(super) changed: usize,
}

pub(super) fn apply(tx: &Connection, diff: &PublicationDiff) -> Result<DocumentChurn> {
    let removed: Vec<i64> = diff.removed.iter().map(|file| file.id).collect();
    let live_ids = |indexes: &[usize]| -> Vec<i64> {
        indexes
            .iter()
            .filter_map(|index| diff.files[*index].previous.as_ref().map(|live| live.id))
            .chain(removed.iter().copied())
            .collect()
    };
    for file_id in live_ids(&diff.dirty_relationships) {
        tx.prepare_cached("DELETE FROM main.relationships WHERE file_id=?1")?
            .execute([file_id])?;
    }
    for file_id in live_ids(&diff.dirty_occurrences) {
        tx.prepare_cached("DELETE FROM main.occurrences WHERE file_id=?1")?
            .execute([file_id])?;
    }
    for (_, rewrite) in &diff.definition_rewrites {
        for id in &rewrite.deleted {
            tx.prepare_cached("DELETE FROM main.definitions WHERE id=?1")?
                .execute([id])?;
        }
    }
    let restaged: Vec<usize> = (0..diff.files.len())
        .filter(|index| diff.files[*index].restaged)
        .collect();
    let mut changed_documents = 0;
    for file_id in live_ids(&restaged) {
        changed_documents += tx
            .prepare_cached("DELETE FROM main.documents WHERE rowid IN (SELECT id FROM main.regions WHERE file_id=?1)")?
            .execute([file_id])?;
        tx.prepare_cached("DELETE FROM main.regions WHERE file_id=?1")?
            .execute([file_id])?;
    }
    for file_id in &removed {
        tx.prepare_cached("DELETE FROM main.definitions WHERE file_id=?1")?
            .execute([file_id])?;
        tx.prepare_cached("DELETE FROM main.files WHERE id=?1")?
            .execute([file_id])?;
    }
    write_files(tx, diff)?;
    write_definitions(tx, diff)?;
    for index in &diff.dirty_occurrences {
        write_occurrences(tx, diff, *index)?;
    }
    for index in &diff.dirty_relationships {
        write_relationships(tx, diff, *index)?;
    }
    for index in &restaged {
        changed_documents += write_regions(tx, diff, *index)?;
    }
    write_contents(tx, diff)?;
    write_extraction_cache(tx, diff)?;
    Ok(DocumentChurn {
        changed: changed_documents,
    })
}

fn write_files(tx: &Connection, diff: &PublicationDiff) -> Result<()> {
    for file in diff.files.iter().filter(|file| !file.row_unchanged()) {
        let [size, mtime, ctime, inode, device] = file.stat;
        tx.prepare_cached(
            "INSERT INTO main.files(id,path,revision,language,status,bytes,stat_size,stat_mtime_ns,stat_ctime_ns,stat_inode,stat_device)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,language=excluded.language,
               status=excluded.status,bytes=excluded.bytes,stat_size=excluded.stat_size,
               stat_mtime_ns=excluded.stat_mtime_ns,stat_ctime_ns=excluded.stat_ctime_ns,
               stat_inode=excluded.stat_inode,stat_device=excluded.stat_device",
        )?
        .execute(params![
            file.live_id,
            file.path,
            file.revision,
            file.language,
            file.status,
            file.bytes,
            size,
            mtime,
            ctime,
            inode,
            device
        ])?;
    }
    Ok(())
}

fn write_definitions(tx: &Connection, diff: &PublicationDiff) -> Result<()> {
    for (index, rewrite) in &diff.definition_rewrites {
        for (id, start, end) in &rewrite.moved {
            tx.prepare_cached("UPDATE main.definitions SET start=?2,end=?3 WHERE id=?1")?
                .execute(params![id, start, end])?;
        }
        let file_id = diff.files[*index].live_id;
        for (staged_id, live_id) in &rewrite.inserted {
            tx.prepare_cached(
                "INSERT INTO main.definitions(id,file_id,name,kind,start,end,container)
                 SELECT ?2,?3,name,kind,start,end,container FROM staged.definitions WHERE id=?1",
            )?
            .execute(params![staged_id, live_id, file_id])?;
        }
    }
    Ok(())
}

fn write_occurrences(tx: &Connection, diff: &PublicationDiff, index: usize) -> Result<()> {
    let file = &diff.files[index];
    let mut select = tx.prepare_cached(
        "SELECT name,start,end,role,target,candidates,provenance FROM staged.occurrences WHERE file_id=?1 ORDER BY id",
    )?;
    let mut insert = tx.prepare_cached(
        "INSERT INTO main.occurrences(file_id,name,start,end,role,target,candidates,provenance) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
    )?;
    let mut candidates = String::new();
    let mut rows = select.query([file.id])?;
    while let Some(row) = rows.next()? {
        translate_candidates(
            &row.get::<_, String>(5)?,
            &diff.definitions,
            &mut candidates,
        )?;
        insert.execute(params![
            file.live_id,
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            diff.definitions.get_optional(row.get(4)?)?,
            candidates,
            row.get::<_, String>(6)?
        ])?;
    }
    Ok(())
}

fn write_relationships(tx: &Connection, diff: &PublicationDiff, index: usize) -> Result<()> {
    let file = &diff.files[index];
    let mut select = tx.prepare_cached(
        "SELECT source,target,kind,start,end,provenance FROM staged.relationships WHERE file_id=?1 ORDER BY rowid",
    )?;
    let mut insert = tx.prepare_cached(
        "INSERT INTO main.relationships(source,target,kind,file_id,start,end,provenance) VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    let mut rows = select.query([file.id])?;
    while let Some(row) = rows.next()? {
        insert.execute(params![
            diff.definitions.get(row.get(0)?)?,
            diff.definitions.get_optional(row.get(1)?)?,
            row.get::<_, String>(2)?,
            file.live_id,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?
        ])?;
    }
    Ok(())
}

/// Inserts a restaged file's regions and their documents under fresh region ids.
fn write_regions(tx: &Connection, diff: &PublicationDiff, index: usize) -> Result<usize> {
    let file = &diff.files[index];
    let mut select = tx.prepare_cached(
        "SELECT r.start,r.end,r.name,r.kind,r.body,d.name,d.expanded,d.body,d.path
         FROM staged.regions r JOIN staged.documents d ON d.id=r.id WHERE r.file_id=?1 ORDER BY r.id",
    )?;
    let mut region = tx.prepare_cached(
        "INSERT INTO main.regions(file_id,start,end,name,kind,body) VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    let mut document = tx.prepare_cached(
        "INSERT INTO main.documents(rowid,name,expanded,body,path) VALUES(?1,?2,?3,?4,?5)",
    )?;
    let mut inserted = 0;
    let mut rows = select.query([file.id])?;
    while let Some(row) = rows.next()? {
        let id = region.insert(params![
            file.live_id,
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?
        ])?;
        document.execute(params![
            id,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?
        ])?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Adds bytes of restaged revisions and prunes revisions no file references.
fn write_contents(tx: &Connection, diff: &PublicationDiff) -> Result<()> {
    let current: HashSet<&str> = diff
        .files
        .iter()
        .filter_map(|file| file.revision.as_deref())
        .collect();
    for revision in diff
        .files
        .iter()
        .filter(|file| file.restaged)
        .filter_map(|file| file.revision.as_deref())
    {
        tx.prepare_cached(
            "INSERT OR IGNORE INTO main.contents(revision,bytes) SELECT revision,bytes FROM staged.contents WHERE revision=?1",
        )?
        .execute([revision])?;
    }
    let superseded = diff
        .files
        .iter()
        .filter_map(|file| file.previous.as_ref())
        .chain(&diff.removed)
        .filter_map(|live| live.revision.as_deref())
        .filter(|revision| !current.contains(revision));
    for revision in superseded {
        tx.prepare_cached("DELETE FROM main.contents WHERE revision=?1")?
            .execute([revision])?;
    }
    Ok(())
}

/// Keeps exactly the current extraction version's rows for current files.
fn write_extraction_cache(tx: &Connection, diff: &PublicationDiff) -> Result<()> {
    let version = extraction_cache_version();
    let needed: HashSet<(&str, &str)> = diff
        .files
        .iter()
        .filter(|file| file.status != "cancelled")
        .filter_map(|file| Some((file.revision.as_deref()?, grammar(&file.path))))
        .collect();
    let mut stale = Vec::new();
    let mut statement = tx.prepare("SELECT revision,grammar,version FROM main.extraction_cache")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let (revision, grammar, row_version): (String, String, i64) =
            (row.get(0)?, row.get(1)?, row.get(2)?);
        if row_version != version || !needed.contains(&(revision.as_str(), grammar.as_str())) {
            stale.push((revision, grammar, row_version));
        }
    }
    for (revision, grammar, row_version) in stale {
        tx.prepare_cached(
            "DELETE FROM main.extraction_cache WHERE revision=?1 AND grammar=?2 AND version=?3",
        )?
        .execute(params![revision, grammar, row_version])?;
    }
    tx.execute_batch(
        "INSERT OR REPLACE INTO main.extraction_cache(revision,grammar,version,facts)
         SELECT s.revision,s.grammar,s.version,s.facts FROM staged.extraction_cache s
         LEFT JOIN main.extraction_cache m
           ON m.revision=s.revision AND m.grammar=s.grammar AND m.version=s.version
         WHERE m.facts IS NOT s.facts;",
    )?;
    Ok(())
}
