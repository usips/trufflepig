use super::*;
use anyhow::ensure;
use rusqlite::{OptionalExtension, params};
use serde_json::json;

pub(super) struct WriteLease(std::fs::File);
impl Drop for WriteLease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
pub(super) fn write_lease(cache: &Path) -> Result<WriteLease> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("publication.lock"))?;
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(WriteLease(file))
}

pub(super) fn schedule(history: &History) -> Result<()> {
    capacity(history)?;
    let time = crate::results::now();
    history.conn.execute(
        "INSERT OR IGNORE INTO views(tip,cutoff,created,updated) VALUES(?1,?2,?3,?3)",
        params![history.tip.as_str(), time - 2 * 365 * 86400, time],
    )?;
    history.conn.execute(
        "UPDATE views SET updated=?2 WHERE tip=?1 AND complete=1",
        params![history.tip.as_str(), time],
    )?;
    Ok(())
}

pub(super) fn initialize(conn: &Connection, repository: &GitRepository) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_millis(250))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-4096;
        PRAGMA max_page_count=524288; PRAGMA wal_autocheckpoint=1024;
        CREATE TABLE IF NOT EXISTS identity(common_dir TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS commits(oid TEXT PRIMARY KEY,parent TEXT,time INTEGER NOT NULL,summary TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS views(tip TEXT PRIMARY KEY,cutoff INTEGER NOT NULL,created INTEGER NOT NULL,updated INTEGER NOT NULL,visited INTEGER NOT NULL DEFAULT 0,complete INTEGER NOT NULL DEFAULT 0,boundary TEXT NOT NULL DEFAULT 'pending',failure TEXT);
        CREATE TABLE IF NOT EXISTS traversal(tip TEXT NOT NULL,depth INTEGER NOT NULL,oid TEXT NOT NULL,eligible INTEGER NOT NULL,PRIMARY KEY(tip,depth));
        CREATE TABLE IF NOT EXISTS changes(before_oid TEXT NOT NULL,after_oid TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(before_oid,after_oid));")?;
    let identity = crate::store::encode_path(&repository.common_dir);
    let previous: Option<String> = conn
        .query_row("SELECT common_dir FROM identity", [], |r| r.get(0))
        .optional()?;
    ensure!(
        previous.as_ref().is_none_or(|p| p == &identity),
        "history_cache_mismatch: cache belongs to another Git common directory"
    );
    if previous.is_none() {
        conn.execute("INSERT INTO identity VALUES(?1)", [identity])?;
    }
    Ok(())
}

pub(super) fn status(history: &History) -> Result<Value> {
    let row = history
        .conn
        .query_row(
            "SELECT cutoff,visited,complete,boundary,failure FROM views WHERE tip=?1",
            [history.tip.as_str()],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)? as usize,
                    r.get::<_, bool>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let (cutoff, visited, complete, boundary, failure) = row.unwrap_or((
        crate::results::now() - 2 * 365 * 86400,
        0,
        false,
        "pending".into(),
        None,
    ));
    Ok(
        json!({"tip":history.tip,"cutoff":cutoff,"max_visited":MAX_COMMITS,"visited":visited,"complete":complete,"coverage":boundary,"failure":failure,"traversal":"first_parent","changes":"computed_on_demand","source_archive":false,"worker_running":worker::is_running(&history.cache).unwrap_or(false)}),
    )
}

pub(super) fn index(history: &mut History) -> Result<Value> {
    let _writer = write_lease(&history.cache)?;
    let time = crate::results::now();
    history.conn.execute(
        "INSERT OR IGNORE INTO views(tip,cutoff,created,updated) VALUES(?1,?2,?3,?3)",
        params![history.tip.as_str(), time - 2 * 365 * 86400, time],
    )?;
    history.conn.execute(
        "UPDATE views SET updated=?2 WHERE tip=?1",
        params![history.tip.as_str(), time],
    )?;
    let result = index_batch(history);
    if let Err(error) = result {
        let category = if error.to_string().contains("resource") {
            "resource_limited"
        } else {
            "unavailable"
        };
        history.conn.execute(
            "UPDATE views SET failure=?2,boundary=?2,updated=?3 WHERE tip=?1",
            params![history.tip.as_str(), category, time],
        )?;
    }
    status(history)
}

fn index_batch(history: &mut History) -> Result<()> {
    let (cutoff, visited, complete): (i64, usize, bool) = history.conn.query_row(
        "SELECT cutoff,visited,complete FROM views WHERE tip=?1",
        [history.tip.as_str()],
        |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize, r.get(2)?)),
    )?;
    if complete {
        return Ok(());
    }
    capacity(history)?;
    let start = if visited == 0 {
        Some(history.tip.to_string())
    } else {
        history.conn.query_row("SELECT c.parent FROM traversal t JOIN commits c ON c.oid=t.oid WHERE t.tip=?1 AND t.depth=?2",params![history.tip.as_str(),visited as i64-1],|r|r.get::<_,Option<String>>(0))?
    };
    let Some(start) = start else {
        history.conn.execute(
            "UPDATE views SET complete=1,boundary='root' WHERE tip=?1",
            [history.tip.as_str()],
        )?;
        return Ok(());
    };
    let max = (129usize.min(MAX_COMMITS.saturating_sub(visited) + 1)).to_string();
    let bytes = history.repository.run(&[
        "log",
        "--first-parent",
        "--no-decorate",
        "-z",
        "--format=%H%x00%P%x00%ct%x00%<(256,trunc)%s",
        "--max-count",
        &max,
        &start,
        "--",
    ])?;
    let fields = bytes.split(|b| *b == 0).collect::<Vec<_>>();
    ensure!(
        fields.len() % 4 == 1 && fields.last() == Some(&&b""[..]),
        "history_unavailable: malformed commit traversal"
    );
    let total = fields.len() / 4;
    let count = 128usize.min(total).min(MAX_COMMITS.saturating_sub(visited));
    let end = visited + count;
    let finished = count == total || end == MAX_COMMITS;
    let shallow = history
        .repository
        .run(&["rev-parse", "--is-shallow-repository"])?
        == b"true\n";
    let last_parent = if shallow && finished && count == total && total > 0 {
        commands::first_parent(
            history,
            &GitOid::parse(std::str::from_utf8(fields[(total - 1) * 4])?)?,
        )?
    } else {
        None
    };
    let boundary = if !finished {
        "indexing"
    } else if end == MAX_COMMITS && total > count {
        "depth_limited"
    } else if last_parent.is_some() {
        "shallow_boundary"
    } else {
        "root"
    };
    let tx = history
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for depth in visited..end {
        let index = depth - visited;
        let row = &fields[index * 4..index * 4 + 4];
        let oid = std::str::from_utf8(row[0])?;
        GitOid::parse(oid)?;
        let parent = std::str::from_utf8(row[1])?
            .split_whitespace()
            .next()
            .or_else(|| {
                (index + 1 == total)
                    .then_some(last_parent.as_ref())
                    .flatten()
                    .map(GitOid::as_str)
            });
        let timestamp: i64 = std::str::from_utf8(row[2])?.parse()?;
        let summary = String::from_utf8_lossy(row[3]);
        tx.execute(
            "INSERT OR IGNORE INTO commits VALUES(?1,?2,?3,?4)",
            params![oid, parent, timestamp, summary],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO traversal VALUES(?1,?2,?3,?4)",
            params![history.tip.as_str(), depth as i64, oid, timestamp >= cutoff],
        )?;
    }
    tx.execute(
        "UPDATE views SET visited=?2,complete=?3,boundary=?4,updated=?5,failure=NULL WHERE tip=?1",
        params![
            history.tip.as_str(),
            end as i64,
            finished,
            boundary,
            crate::results::now()
        ],
    )?;
    let obsolete: Option<String> = tx
        .query_row(
            "SELECT tip FROM views WHERE updated<?1 ORDER BY updated LIMIT 1",
            [crate::results::now() - 30 * 86400],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(tip) = obsolete {
        tx.execute("DELETE FROM traversal WHERE tip=?1", [&tip])?;
        tx.execute("DELETE FROM views WHERE tip=?1", [&tip])?;
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn capacity(history: &History) -> Result<()> {
    checkpoint(history)?;
    let pages: u64 = history
        .conn
        .query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))
        .map(|n| n as u64)?;
    loop {
        let free: u64 = history
            .conn
            .query_row("PRAGMA freelist_count", [], |r| r.get::<_, i64>(0))
            .map(|n| n as u64)?;
        if pages.saturating_sub(free) * 4096 < MAX_DATABASE_BYTES - 8 * 1024 * 1024 {
            break;
        }
        let deleted = history.conn.execute(
            "DELETE FROM changes WHERE rowid=(SELECT rowid FROM changes LIMIT 1)",
            [],
        )?;
        ensure!(
            deleted > 0,
            "history_resource_limited: database capacity reached"
        );
        checkpoint(history)?;
    }
    Ok(())
}

fn checkpoint(history: &History) -> Result<()> {
    let wal = history.cache.join("history.sqlite3-wal");
    if std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0) >= MAX_WAL_BYTES - 32 * 1024 * 1024 {
        let (busy, _, _): (i64, i64, i64) =
            history
                .conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
        ensure!(
            busy == 0,
            "history_resource_limited: WAL checkpoint is blocked"
        );
    }
    Ok(())
}
