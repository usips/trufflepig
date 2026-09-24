use super::hit_row;
use crate::{
    output::OutputBudget,
    results::{self, DefinitionTarget, Hit, MAX_HITS, ResultSet},
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
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
    super::snippets::attach(store, &super::snippets::preview_terms(name), &mut hits)?;
    snapshot.commit()?;
    Ok(ResultSet {
        generation,
        coverage,
        truncated,
        hits,
    })
}

pub fn context(store: &Store, handle: &str, budget: &OutputBudget) -> Result<String> {
    let (generation, hit) = results::handle(store, handle)?;
    context_entry(store, generation, hit, budget, &serde_json::json!({}))
}

pub(crate) fn context_entry(
    store: &Store,
    generation: i64,
    hit: Hit,
    budget: &OutputBudget,
    metadata: &serde_json::Value,
) -> Result<String> {
    let metadata = metadata
        .as_object()
        .context("invalid_metadata: context metadata must be an object")?;
    let snapshot = store.conn.unchecked_transaction()?;
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
        let mut response = serde_json::json!({"generation":generation,"hit":hit,"relationships":edges,"truncated":edges.len()<total,"tokenizer":"o200k_base"});
        let object = response.as_object_mut().expect("context response object");
        ensure!(
            metadata.keys().all(|key| !object.contains_key(key)),
            "invalid_metadata: context metadata collides with response identity"
        );
        object.extend(metadata.clone());
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

/// Outline of modules and types under a path prefix; a prefix naming exactly one
/// file also lists its functions, methods, constants, and macros.
pub fn map(store: &Store, path: &str) -> Result<ResultSet> {
    let snapshot = store.conn.unchecked_transaction()?;
    let generation = store.generation()?;
    let coverage = serde_json::to_value(store.coverage()?)?;
    let mut stmt=store.conn.prepare("SELECT f.path,f.revision,d.start,d.end,d.name,d.kind,d.container,'structural_map',c.bytes FROM definitions d JOIN files f ON f.id=d.file_id JOIN contents c ON c.revision=f.revision WHERE substr(f.path,1,length(?1))=?1 AND (d.kind IN ('module','struct','class','trait','type','enum','impl','interface') OR (f.path=?1 AND d.kind IN ('function','method','constant','macro'))) ORDER BY f.path,CASE d.kind WHEN 'module' THEN 0 ELSE 1 END,d.start,d.id LIMIT ?2")?;
    let mut hits = stmt
        .query_map(params![path, (MAX_HITS + 1) as i64], hit_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = hits.len() > MAX_HITS;
    hits.truncate(MAX_HITS);
    drop(stmt);
    super::snippets::attach(store, &[], &mut hits)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_context_preserves_provenance_and_rejects_generation_drift() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lib.rs"), "struct Engine {}\n").unwrap();
        let mut store = Store::open(root.path(), cache.path()).unwrap();
        store.index().unwrap();
        let mut set = map(&store, "").unwrap();
        let mut hit = set.hits.pop().unwrap();
        hit.handle = format!("{}:1", uuid::Uuid::new_v4());
        let metadata = serde_json::json!({"member":"engine"});
        let budget = OutputBudget::new(600).unwrap();
        let output =
            context_entry(&store, set.generation, hit.clone(), &budget, &metadata).unwrap();
        assert!(budget.fits(&output));
        let response: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(response["member"], "engine");
        assert_eq!(response["hit"]["handle"], hit.handle);

        assert!(
            context_entry(
                &store,
                set.generation,
                hit.clone(),
                &OutputBudget::new(1).unwrap(),
                &metadata
            )
            .unwrap_err()
            .to_string()
            .contains("budget_too_small")
        );
        std::fs::write(root.path().join("lib.rs"), "struct Replacement {}\n").unwrap();
        store.index().unwrap();
        assert!(
            context_entry(&store, set.generation, hit, &budget, &metadata)
                .unwrap_err()
                .to_string()
                .contains("stale_result")
        );
    }
}
