//! The publication journal: per-generation observations of edits, additions,
//! deletions, and coverage changes, bounded by window, count, and bytes
//! ([coherent publication](../../../docs/index-contract.md#coherent-publication)).
use crate::identity::ContentRevision;
use crate::{
    path_codec::decode_path,
    store::{Publication, PublicationChange, PublicationObservations},
};
use anyhow::Result;
use rusqlite::{Connection, params};

const MAX_OBSERVATIONS: usize = 4096;
const MAX_JOURNAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_WINDOWS: i64 = 256;

pub(in crate::store) fn create_journal(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS publication_windows(generation INTEGER PRIMARY KEY,complete INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS publication_observations(generation INTEGER NOT NULL,path TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(generation,path));"
    )?;
    Ok(())
}

pub(super) fn append_observations(conn: &Connection, publication: &Publication) -> Result<()> {
    let root = decode_path(&publication.root)?;
    let mut query = conn.prepare(
        "SELECT f.path,f.revision,n.revision,f.status,n.status FROM files f LEFT JOIN staged.files n USING(path)
         WHERE n.path IS NULL OR f.revision IS NOT n.revision OR f.status<>n.status OR f.bytes<>n.bytes
         UNION ALL SELECT n.path,NULL,n.revision,NULL,n.status FROM staged.files n LEFT JOIN files f USING(path) WHERE f.path IS NULL
         ORDER BY 1 LIMIT ?1"
    )?;
    let mut rows = query.query([(MAX_OBSERVATIONS + 1) as i64])?;
    let mut complete = true;
    let mut bytes = 0;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let before: Option<String> = row.get(1)?;
        let after: Option<String> = row.get(2)?;
        let before_status: Option<String> = row.get(3)?;
        let after_status: Option<String> = row.get(4)?;
        let kind = if before_status.is_none() {
            "added"
        } else if after_status.is_none() {
            match std::fs::symlink_metadata(root.join(decode_path(&path)?)) {
                Ok(_) => "ignored_or_excluded",
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => "deleted",
                Err(_) => "coverage_unknown",
            }
        } else if before_status != after_status || before.is_some() != after.is_some() {
            "coverage_changed"
        } else {
            "modified"
        };
        let change = PublicationChange {
            generation: publication.generation,
            path: path.clone(),
            before_revision: before.as_deref().map(ContentRevision::parse).transpose()?,
            after_revision: after.as_deref().map(ContentRevision::parse).transpose()?,
            before_status,
            after_status,
            kind: kind.into(),
        };
        let value = serde_json::to_string(&change)?;
        bytes += value.len() + path.len();
        if count == MAX_OBSERVATIONS || bytes > MAX_JOURNAL_BYTES {
            complete = false;
            break;
        }
        conn.prepare_cached(
            "INSERT INTO publication_observations(generation,path,value) VALUES(?1,?2,?3)",
        )?
        .execute(params![publication.generation, path, value])?;
        count += 1;
    }
    conn.execute(
        "INSERT INTO publication_windows(generation,complete) VALUES(?1,?2)",
        params![publication.generation, complete],
    )?;
    conn.execute(
        "DELETE FROM publication_windows WHERE generation<=?1",
        [publication.generation - MAX_WINDOWS],
    )?;
    conn.execute("DELETE FROM publication_observations WHERE generation NOT IN (SELECT generation FROM publication_windows)", [])?;
    loop {
        let (count, bytes): (i64, i64) = conn.query_row(
            "SELECT count(*),coalesce(sum(length(CAST(value AS BLOB))+length(CAST(path AS BLOB))),0) FROM publication_observations",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if count <= MAX_OBSERVATIONS as i64 && bytes <= MAX_JOURNAL_BYTES as i64 {
            break;
        }
        // Evict entire generations so readers can detect an incomplete window.
        conn.execute("DELETE FROM publication_windows WHERE generation=(SELECT min(generation) FROM publication_windows)", [])?;
        conn.execute("DELETE FROM publication_observations WHERE generation NOT IN (SELECT generation FROM publication_windows)", [])?;
    }
    Ok(())
}

pub(in crate::store) fn observations_since(
    conn: &Connection,
    from: i64,
    to: i64,
) -> Result<PublicationObservations> {
    anyhow::ensure!(
        from >= 0 && from <= to,
        "invalid publication observation generation"
    );
    let floor: i64 = conn.query_row(
        "SELECT coalesce(min(generation),?1) FROM publication_windows",
        [to + 1],
        |row| row.get(0),
    )?;
    let (windows, complete): (i64, bool) = conn.query_row(
        "SELECT count(*),coalesce(min(complete),1) FROM publication_windows WHERE generation>?1 AND generation<=?2",
        params![from, to], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM publication_observations WHERE generation>?1 AND generation<=?2",
        params![from, to],
        |row| row.get(0),
    )?;
    let mut changes = Vec::with_capacity(count.try_into()?);
    let mut statement = conn.prepare("SELECT value FROM publication_observations WHERE generation>?1 AND generation<=?2 ORDER BY generation,path")?;
    for value in statement.query_map(params![from, to], |row| row.get::<_, String>(0))? {
        changes.push(serde_json::from_str(&value?)?);
    }
    Ok(PublicationObservations {
        from_generation: from,
        to_generation: to,
        retained_generation_floor: floor,
        complete: complete && windows == to - from,
        changes,
    })
}
