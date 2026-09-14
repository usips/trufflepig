use super::hit_row;
use crate::{
    output::OutputBudget,
    results::{self, DefinitionTarget, MAX_HITS, ResultSet},
    store::Store,
};
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};

pub fn references(store: &Store, name: &str) -> Result<ResultSet> {
    let snapshot = store.conn.unchecked_transaction()?;
    let generation = store.generation()?;
    let coverage = serde_json::to_value(store.coverage()?)?;
    let mut stmt=store.conn.prepare("SELECT f.path,f.revision,o.start,o.end,o.name,o.role,NULL,o.provenance,c.bytes,o.target,o.candidates FROM occurrences o JOIN files f ON f.id=o.file_id JOIN contents c ON c.revision=f.revision WHERE o.name=?1 ORDER BY f.path,o.start,o.id LIMIT ?2")?;
    let rows = stmt.query_map(params![name, (MAX_HITS + 1) as i64], |r| {
        Ok((
            hit_row(r)?,
            r.get::<_, Option<i64>>(9)?,
            r.get::<_, String>(10)?,
        ))
    })?;
    let mut hits = Vec::with_capacity(128);
    let mut bytes = 0usize;
    let mut truncated = false;
    for row in rows {
        let (mut hit, target, candidates) = row?;
        let candidates: Vec<i64> = serde_json::from_str(&candidates)?;
        hit.resolution = Some(
            if target.is_some() {
                "resolved"
            } else if !candidates.is_empty() {
                "candidate"
            } else {
                "unresolved"
            }
            .into(),
        );
        hit.target = target.map(|id| definition_target(store, id)).transpose()?;
        for id in candidates.into_iter().filter(|id| Some(*id) != target) {
            hit.candidates.push(definition_target(store, id)?);
        }
        bytes += serde_json::to_vec(&hit)?.len();
        if bytes > results::MAX_BYTES {
            truncated = true;
            break;
        }
        hits.push(hit);
    }
    truncated |= hits.len() > MAX_HITS;
    hits.truncate(MAX_HITS);
    drop(stmt);
    snapshot.commit()?;
    Ok(ResultSet {
        generation,
        coverage,
        truncated,
        hits,
    })
}

pub fn context(store: &Store, handle: &str, budget: &OutputBudget) -> Result<String> {
    let snapshot = store.conn.unchecked_transaction()?;
    let (generation, hit) = results::handle(store, handle)?;
    if generation != store.generation()? {
        bail!("stale_result: graph generation changed; search again");
    }
    let current_revision: Option<String> = store
        .conn
        .query_row(
            "SELECT revision FROM files WHERE path=?1",
            [&hit.path],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if current_revision != hit.revision || current_revision.is_none() {
        bail!("stale_result: result source is outside current graph revision; search again");
    }
    let mut stmt=store.conn.prepare("SELECT r.kind,r.provenance,f.path,r.start,r.end,r.source,r.target FROM relationships r JOIN files f ON f.id=r.file_id WHERE r.source IN (SELECT d.id FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.path=?1 AND d.start<=?2 AND d.end>=?3) OR r.target IN (SELECT d.id FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.path=?1 AND d.start<=?2 AND d.end>=?3) ORDER BY f.path,r.start,r.kind,r.source,r.target LIMIT 101")?;
    let rows = stmt.query_map(params![hit.path, hit.start as i64, hit.end as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, Option<i64>>(6)?,
        ))
    })?;
    let mut edges = Vec::with_capacity(101);
    for row in rows {
        let (kind, provenance, path, start, end, source, target) = row?;
        let resolution = if kind.ends_with("_candidate") {
            "candidate"
        } else if target.is_some() {
            "resolved"
        } else {
            "unresolved"
        };
        edges.push(serde_json::json!({"kind":kind,"resolution":resolution,"provenance":provenance,"path":path,"start":start,"end":end,"source":definition_target(store,source)?,"target":target.map(|id|definition_target(store,id)).transpose()?}));
    }
    let total = edges.len();
    edges.truncate(100);
    loop {
        let response = serde_json::json!({"generation":generation,"hit":hit,"relationships":edges,"truncated":edges.len()<total,"tokenizer":"o200k_base"});
        let text = budget.encode(&response)?;
        if budget.fits(&text) {
            drop(stmt);
            snapshot.commit()?;
            return Ok(text);
        }
        if edges.pop().is_none() {
            bail!("budget_too_small: context envelope does not fit");
        }
    }
}

pub fn map(store: &Store, path: &str) -> Result<ResultSet> {
    let snapshot = store.conn.unchecked_transaction()?;
    let generation = store.generation()?;
    let coverage = serde_json::to_value(store.coverage()?)?;
    let mut stmt=store.conn.prepare("SELECT f.path,f.revision,d.start,d.end,d.name,d.kind,d.container,'structural_map',c.bytes FROM definitions d JOIN files f ON f.id=d.file_id JOIN contents c ON c.revision=f.revision WHERE substr(f.path,1,length(?1))=?1 AND d.kind IN ('module','struct','class','trait','type','enum','impl') ORDER BY f.path,CASE d.kind WHEN 'module' THEN 0 ELSE 1 END,d.start,d.id LIMIT ?2")?;
    let mut hits = stmt
        .query_map(params![path, (MAX_HITS + 1) as i64], hit_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = hits.len() > MAX_HITS;
    hits.truncate(MAX_HITS);
    drop(stmt);
    snapshot.commit()?;
    Ok(ResultSet {
        generation,
        coverage,
        truncated,
        hits,
    })
}

fn definition_target(store: &Store, id: i64) -> Result<DefinitionTarget> {
    Ok(store.conn.query_row("SELECT f.path,f.revision,d.start,d.end,d.name FROM definitions d JOIN files f ON f.id=d.file_id WHERE d.id=?1",[id],|r|Ok(DefinitionTarget{path:r.get(0)?,revision:r.get(1)?,start:r.get::<_,i64>(2)? as usize,end:r.get::<_,i64>(3)? as usize,name:r.get(4)?}))?)
}
