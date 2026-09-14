//! Immutable, persisted query snapshots. Missing IDs never resolve through newer sets.
use crate::{
    identity::{ResultCursor, ResultHandle},
    output::OutputBudget,
    store::Store,
};
mod entries;
use anyhow::{Context, Result, bail};
pub use entries::*;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SETS: usize = 50;
pub const MAX_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_HITS: usize = 10_000;
const TTL_SECONDS: i64 = 600;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DefinitionTarget {
    pub path: String,
    pub revision: String,
    pub start: usize,
    pub end: usize,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hit {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handle: String,
    pub path: String,
    pub revision: Option<String>,
    pub start: usize,
    pub end: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<DefinitionTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<DefinitionTarget>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResultSet {
    pub generation: i64,
    pub coverage: Value,
    pub truncated: bool,
    pub hits: Vec<Hit>,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn initialize(store: &Store) -> Result<()> {
    store.conn.execute_batch("CREATE TABLE IF NOT EXISTS result_sets(id TEXT PRIMARY KEY, created INTEGER NOT NULL, expires INTEGER NOT NULL, payload TEXT NOT NULL)")?;
    Ok(())
}

pub fn save(store: &mut Store, set: ResultSet) -> Result<String> {
    save_entries(
        store,
        set.generation,
        set.coverage,
        set.hits.into_iter().map(ResultEntry::LiveSource).collect(),
        set.truncated,
    )
}

pub fn save_entries(
    store: &Store,
    generation: i64,
    coverage: Value,
    entries: Vec<ResultEntry>,
    truncated: bool,
) -> Result<String> {
    let mut set = StoredResultSet {
        generation,
        coverage,
        hits: entries,
        truncated,
    };
    initialize(store)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    if set.hits.len() > MAX_HITS {
        set.hits.truncate(MAX_HITS);
        set.truncated = true;
    }
    for (ordinal, hit) in set.hits.iter_mut().enumerate() {
        *hit.handle_mut() = format!("{id}:{}", ordinal + 1);
    }
    let mut payload = serde_json::to_string(&set)?;
    while payload.len() > MAX_BYTES && !set.hits.is_empty() {
        let keep = (set.hits.len() * MAX_BYTES / payload.len()).saturating_sub(1);
        set.hits.truncate(keep);
        set.truncated = true;
        payload = serde_json::to_string(&set)?;
    }
    if payload.len() > MAX_BYTES {
        bail!("result_cache_unavailable: metadata exceeds cache capacity");
    }
    let tx = rusqlite::Transaction::new_unchecked(
        &store.conn,
        rusqlite::TransactionBehavior::Immediate,
    )?;
    let time = now();
    tx.execute("DELETE FROM result_sets WHERE expires<=?1", [time])?;
    tx.execute(
        "INSERT INTO result_sets VALUES(?1,?2,?3,?4)",
        params![id, time, time + TTL_SECONDS, payload],
    )?;
    loop {
        let (count, bytes): (usize, usize) = tx.query_row(
            "SELECT count(*),coalesce(sum(length(CAST(payload AS BLOB))),0) FROM result_sets",
            [],
            |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize)),
        )?;
        if count <= MAX_SETS && bytes <= MAX_BYTES {
            break;
        }
        tx.execute("DELETE FROM result_sets WHERE id=(SELECT id FROM result_sets WHERE id != ?1 ORDER BY created,id LIMIT 1)", [&id])?;
    }
    tx.commit()?;
    Ok(id)
}

pub fn load(store: &Store, id: &str) -> Result<ResultSet> {
    let set = load_entries(store, id)?;
    let hits = set
        .hits
        .into_iter()
        .map(|entry| match entry {
            ResultEntry::LiveSource(hit) => Ok(hit),
            _ => bail!("historical_result: expected live-source result set"),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ResultSet {
        generation: set.generation,
        coverage: set.coverage,
        truncated: set.truncated,
        hits,
    })
}

pub fn load_entries(store: &Store, id: &str) -> Result<StoredResultSet> {
    uuid::Uuid::parse_str(id)
        .context("invalid_handle: expected immutable result-set identifier")?;
    initialize(store)?;
    let row: Option<(i64, String)> = store
        .conn
        .query_row(
            "SELECT expires,payload FROM result_sets WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((expires, payload)) = row else {
        bail!("expired_result: result set expired or was evicted; search again");
    };
    if expires <= now() {
        bail!("expired_result: result set expired; search again");
    }
    serde_json::from_str(&payload)
        .context("result_unavailable: stored result format is invalid; search again")
}

pub fn handle(store: &Store, handle: &str) -> Result<(i64, Hit)> {
    let (generation, entry) = entry(store, handle)?;
    match entry {
        ResultEntry::LiveSource(hit) => Ok((generation, hit)),
        _ => bail!("historical_result: historical entries are invalid for live context"),
    }
}

pub fn entry(store: &Store, handle: &str) -> Result<(i64, ResultEntry)> {
    let handle: ResultHandle = handle.parse()?;
    let set = load_entries(store, &handle.set.simple().to_string())?;
    let entry = set
        .hits
        .get(handle.ordinal - 1)
        .context("invalid_handle: ordinal outside result set")?;
    Ok((set.generation, entry.clone()))
}

pub fn page(
    store: &Store,
    id: &str,
    offset: usize,
    limit: usize,
    budget: &OutputBudget,
) -> Result<String> {
    let set = load_entries(store, id)?;
    if offset > set.hits.len() {
        bail!("invalid_cursor: offset outside result set");
    }
    let available = set.hits.len() - offset;
    let mut count = available.min(limit);
    loop {
        let next = if offset + count < set.hits.len() {
            Some(format!("{id}@{}", offset + count))
        } else {
            None
        };
        let mut value = serde_json::json!({"generation":set.generation,"coverage":set.coverage,"tokenizer":"o200k_base","hits":&set.hits[offset..offset+count],"next":next,"truncated":set.truncated});
        let text = budget.encode(&value)?;
        if budget.fits(&text) && (count > 0 || available == 0) {
            return Ok(text);
        }
        if count == 1
            && let Some(candidates) = value["hits"][0]["candidates"].as_array()
        {
            let total = candidates.len();
            let mut keep = total / 2;
            while keep > 0 {
                value["hits"][0]["candidates"]
                    .as_array_mut()
                    .expect("candidate array")
                    .truncate(keep);
                value["hits"][0]["candidates_total"] = total.into();
                value["hits"][0]["candidates_truncated"] = true.into();
                let text = budget.encode(&value)?;
                if budget.fits(&text) {
                    return Ok(text);
                }
                keep /= 2;
            }
        }
        if count == 0 {
            bail!("budget_too_small: no hit and pagination envelope fit; increase --budget");
        }
        count /= 2;
    }
}

pub fn more(store: &Store, cursor: &str, limit: usize, budget: &OutputBudget) -> Result<String> {
    let cursor: ResultCursor = cursor.parse()?;
    page(
        store,
        &cursor.set.simple().to_string(),
        cursor.offset,
        limit,
        budget,
    )
}

#[cfg(test)]
mod tests;
