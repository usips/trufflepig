//! Explicit observational sessions retain fingerprint baselines independently of the live index.

mod session_snapshot;
#[cfg(test)]
mod tests;

use crate::store::{PublicationObservations, Store};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use session_snapshot::SessionSnapshot;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_OPEN_SESSIONS: i64 = 50;
const MAX_BASELINE_BYTES: u64 = 64 * 1024 * 1024;
const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS diagnostic_sessions(
            id TEXT PRIMARY KEY,started INTEGER NOT NULL,ended INTEGER,
            status TEXT NOT NULL,baseline TEXT,report TEXT,overlap INTEGER NOT NULL DEFAULT 0);
         CREATE INDEX IF NOT EXISTS diagnostic_session_status ON diagnostic_sessions(status);",
    )?;
    expire(conn)
}

fn expire(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE diagnostic_sessions SET status='expired_incomplete',baseline=NULL,ended=?2 WHERE status='open' AND started<?1",
        params![now().saturating_sub(RETENTION_SECONDS),now()],
    )?;
    // An expiry is a new incomplete-session notice; its baseline facts are already erased.
    conn.execute(
        "DELETE FROM diagnostic_sessions WHERE ended<?1",
        [now().saturating_sub(RETENTION_SECONDS)],
    )?;
    Ok(())
}

pub fn baseline_bytes(conn: &Connection) -> Result<u64> {
    Ok(conn.query_row(
        "SELECT coalesce(sum(length(CAST(baseline AS BLOB))),0) FROM diagnostic_sessions",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64)
}

pub fn start(conn: &Connection, store: &mut Store) -> Result<Value> {
    expire(conn)?;
    let open: i64 = conn.query_row(
        "SELECT count(*) FROM diagnostic_sessions WHERE status='open'",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        open < MAX_OPEN_SESSIONS,
        "at most 50 diagnostic sessions may remain open"
    );
    let baseline = session_snapshot::capture(
        store,
        MAX_BASELINE_BYTES.saturating_sub(baseline_bytes(conn)?),
    )?;
    let encoded = serde_json::to_string(&baseline)?;
    let transaction = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let open: i64 = transaction.query_row(
        "SELECT count(*) FROM diagnostic_sessions WHERE status='open'",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        open < MAX_OPEN_SESSIONS,
        "at most 50 diagnostic sessions may remain open"
    );
    ensure!(
        baseline_bytes(&transaction)?.saturating_add(encoded.len() as u64) <= MAX_BASELINE_BYTES,
        "session baseline capacity exhausted"
    );
    let id = uuid::Uuid::new_v4().simple().to_string();
    transaction.execute(
        "UPDATE diagnostic_sessions SET overlap=1 WHERE status='open'",
        [],
    )?;
    transaction.execute("INSERT INTO diagnostic_sessions(id,started,status,baseline,overlap) VALUES(?1,?2,'open',?3,?4)", params![id,now(),encoded,open>0])?;
    transaction.commit()?;
    Ok(
        json!({"session":id,"status":"open","baseline_generation":baseline.generation,"capture_interval":[baseline.capture_start,baseline.capture_end],"baseline_bytes":encoded.len(),"overlapping_observation":open>0,"ownership":"observational_only"}),
    )
}

pub fn end(conn: &Connection, store: &mut Store, id: &str) -> Result<Value> {
    expire(conn)?;
    let value: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT status,baseline FROM diagnostic_sessions WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (status, baseline) = value.ok_or_else(|| anyhow::anyhow!("unknown diagnostic session"))?;
    ensure!(status == "open", "diagnostic session is {status}");
    let before: SessionSnapshot = serde_json::from_str(
        &baseline.ok_or_else(|| anyhow::anyhow!("session baseline unavailable"))?,
    )?;
    let after = session_snapshot::capture(store, MAX_BASELINE_BYTES)?;
    let index_replaced =
        before.index_epoch != after.index_epoch || before.generation > after.generation;
    let observed = if index_replaced {
        None
    } else {
        store.observations_since(before.generation).ok()
    };
    let observations_unavailable = observed.is_none();
    let mut observations = observed.unwrap_or_else(|| PublicationObservations {
        from_generation: before.generation,
        to_generation: after.generation,
        retained_generation_floor: after.generation,
        complete: false,
        changes: Vec::new(),
    });
    observations
        .changes
        .retain(|change| change.generation <= after.generation);
    observations.to_generation = after.generation;
    let mut report = session_snapshot::compare(&before, &after, &observations)?;
    report["publication_index_replaced"] = json!(index_replaced);
    report["publication_observations_unavailable"] = json!(observations_unavailable);
    report["before_index_epoch"] = json!(before.index_epoch);
    report["after_index_epoch"] = json!(after.index_epoch);
    let observations_truncated = observations.changes.len().saturating_sub(128);
    observations.changes.truncate(128);
    report["publication_observations_truncated"] = json!(observations_truncated);
    report["publication_observations"] = serde_json::to_value(observations)?;
    report["intermediate_edits"] =
        json!("published generations observed; unpublished edits cannot be observed");
    report["session"] = json!(id);
    report["status"] = json!("ended");
    let transaction = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let overlap: bool = transaction.query_row(
        "SELECT overlap FROM diagnostic_sessions WHERE id=?1",
        [id],
        |row| row.get(0),
    )?;
    report["overlapping_observation"] = json!(overlap);
    let updated = transaction.execute("UPDATE diagnostic_sessions SET status='ended',ended=?1,baseline=NULL,report=?2 WHERE id=?3 AND status='open'", params![now(),serde_json::to_string(&report)?,id])?;
    ensure!(
        updated == 1,
        "diagnostic session was concurrently ended or invalidated"
    );
    transaction.commit()?;
    Ok(report)
}

pub fn audit(conn: &Connection, id: Option<&str>) -> Result<Value> {
    expire(conn)?;
    let mut query = conn.prepare("SELECT id,started,ended,status,overlap,report FROM diagnostic_sessions WHERE (?1 IS NULL OR id=?1) ORDER BY started DESC,id LIMIT 1000")?;
    let mut rows = query.query([id])?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next()? {
        let report: Option<String> = if id.is_some() { row.get(5)? } else { None };
        sessions.push(json!({"session":row.get::<_,String>(0)?,"started":row.get::<_,i64>(1)?,"ended":row.get::<_,Option<i64>>(2)?,"status":row.get::<_,String>(3)?,"overlapping_observation":row.get::<_,bool>(4)?,"report":report.map(|value|serde_json::from_str::<Value>(&value)).transpose()?}));
    }
    ensure!(
        id.is_none() || !sessions.is_empty(),
        "unknown diagnostic session"
    );
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM diagnostic_sessions WHERE (?1 IS NULL OR id=?1)",
        [id],
        |row| row.get(0),
    )?;
    Ok(
        json!({"sessions":sessions,"sessions_truncated":count.saturating_sub(sessions.len() as i64),"baseline_bytes":baseline_bytes(conn)?,"metrics":"observational proxies; no ownership or task-success attribution"}),
    )
}

pub fn forget(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM diagnostic_sessions", [])?;
    Ok(())
}
