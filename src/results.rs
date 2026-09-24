//! Immutable, persisted query snapshots. Missing IDs never resolve through newer sets.
use crate::{identity::ResultHandle, store::Store};
mod entries;
pub(crate) mod lines;
mod page;
use anyhow::{Context, Result, bail, ensure};
pub use entries::*;
pub(crate) use page::{largest_fitting, snippet_page};
pub use page::{more, page};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Component, Path},
    time::{SystemTime, UNIX_EPOCH},
};

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
    /// Preview of the most relevant line inside the span, captured at search time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<Snippet>,
}

/// One trimmed source line (at most 120 characters) previewing a hit. It locates
/// evidence; it is not a verified read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResultSet {
    pub generation: i64,
    pub coverage: Value,
    pub truncated: bool,
    pub hits: Vec<Hit>,
}

const COMPACT_NAME_LIMIT: usize = 96;

/// Optional parts of a compact hit, dropped (snippets first, then names) when a
/// page would otherwise not fit its budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HitDetail {
    pub names: bool,
    pub snippets: bool,
}

/// Render the locator portion of an immutable live hit, plus its snippet.
///
/// The stored hit keeps revisions, byte spans, and resolver evidence for
/// follow-up commands; pages expose coordinates and a one-line preview.
pub(crate) fn compact_hit(
    hit: &Hit,
    root: &Path,
    member: Option<&str>,
    detail: HitDetail,
) -> Result<Value> {
    let names = detail.names;
    let mut value = serde_json::Map::new();
    if !hit.handle.is_empty() {
        value.insert("handle".into(), hit.handle.clone().into());
    }
    if let Some(member) = member {
        value.insert("member".into(), member.into());
    }
    value.insert("file".into(), file_uri(root, &hit.path)?.into());
    value.insert("start_line".into(), hit.start_line.into());
    value.insert("end_line".into(), hit.end_line.into());
    if names && concise_name(&hit.name) {
        value.insert("name".into(), hit.name.clone().into());
    }
    if detail.snippets
        && let Some(snippet) = &hit.snippet
    {
        value.insert("snippet".into(), serde_json::to_value(snippet)?);
    }
    if let Some(resolution) = &hit.resolution {
        value.insert("resolution".into(), resolution.clone().into());
    }
    if let Some(target) = &hit.target {
        value.insert("target".into(), compact_target(target, root, names)?);
    }
    if !hit.candidates.is_empty() {
        value.insert(
            "candidates".into(),
            hit.candidates
                .iter()
                .map(|target| compact_target(target, root, names))
                .collect::<Result<Vec<_>>>()?
                .into(),
        );
    }
    Ok(Value::Object(value))
}

/// Render one persisted entry for a result page without changing its cache.
pub(crate) fn compact_entry(
    entry: &ResultEntry,
    root: &Path,
    member: Option<&str>,
    detail: HitDetail,
) -> Result<Value> {
    compact_entry_for_page(entry, root, member, detail, true)
}

/// Render an entry according to the result set's output contract.
pub(crate) fn compact_entry_for_page(
    entry: &ResultEntry,
    root: &Path,
    member: Option<&str>,
    detail: HitDetail,
    compact_live: bool,
) -> Result<Value> {
    match entry {
        ResultEntry::LiveSource(hit) if compact_live => compact_hit(hit, root, member, detail),
        _ => Ok(serde_json::to_value(entry)?),
    }
}

fn compact_target(target: &DefinitionTarget, root: &Path, names: bool) -> Result<Value> {
    let mut value = serde_json::Map::new();
    value.insert("file".into(), file_uri(root, &target.path)?.into());
    value.insert("start".into(), target.start.into());
    value.insert("end".into(), target.end.into());
    if names && concise_name(&target.name) {
        value.insert("name".into(), target.name.clone().into());
    }
    Ok(Value::Object(value))
}

pub(crate) fn concise_name(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= COMPACT_NAME_LIMIT
}

/// Return an absolute `file:` URI while preserving raw Unix path bytes.
pub(crate) fn file_uri(root: &Path, relative: &str) -> Result<String> {
    let relative = crate::store::decode_path(relative)?;
    ensure!(
        relative.is_relative(),
        "result_unavailable: stored path is absolute"
    );
    ensure!(
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_))),
        "result_unavailable: stored path escapes repository root"
    );
    let encoded = crate::store::encode_path(&root.join(relative));
    // `?` and `#` delimit URI query/fragment components, so escape them even
    // though they are safe in the root-relative CLI path encoding.
    let encoded = encoded.replace('?', "%3F").replace('#', "%23");
    Ok(format!("file://{encoded}"))
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

#[cfg(test)]
mod tests;
