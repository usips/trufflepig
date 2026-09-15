//! Cached vectors join current source bodies; retrieval never embeds source regions.
use super::{
    Query,
    telemetry::{Lane, LaneOutcome, RetrievalTrace},
};
use crate::{results::Hit, semantic::Embedding, store::Store};
use anyhow::Result;
use std::path::Path;

#[cfg(test)]
mod tests;

pub(super) fn append(
    store: &Store,
    query: &Query,
    cache: &Path,
    vector: &Embedding,
    hits: &mut Vec<Hit>,
    coverage: &mut serde_json::Value,
    trace: &mut RetrievalTrace,
) -> Result<bool> {
    let started = std::time::Instant::now();
    let mut semantic_hits = Vec::new();
    // Retrieve into a staging value so a cache/SQLite failure cannot leave
    // callers with half-written semantic coverage or lose lexical results.
    let mut semantic_coverage = coverage.clone();
    let outcome = retrieve(
        store,
        query,
        cache,
        vector,
        &mut semantic_hits,
        &mut semantic_coverage,
    );
    match outcome {
        Ok(truncated) => {
            trace.record(
                Lane::Semantic,
                started,
                &semantic_hits,
                if semantic_coverage["semantic_pending"].as_u64().unwrap_or(0) > 0 {
                    LaneOutcome::Partial
                } else {
                    LaneOutcome::Complete
                },
                truncated,
            );
            *coverage = semantic_coverage;
            if let Some(generation) = trace.generation {
                trace.snapshot(generation, coverage);
            }
            let lexical = std::mem::take(hits);
            *hits = super::file_ranking::fuse_file_lanes(vec![lexical, semantic_hits]);
            Ok(truncated)
        }
        Err(error) => {
            coverage["semantic_status"] = "partial".into();
            coverage["semantic_reason"] = format!("{error:#}").into();
            let failures = coverage["semantic_failures"].as_u64().unwrap_or(0);
            coverage["semantic_failures"] = failures.saturating_add(1).into();
            trace.record(Lane::Semantic, started, &[], LaneOutcome::Failed, false);
            if let Some(generation) = trace.generation {
                trace.snapshot(generation, coverage);
            }
            Ok(false)
        }
    }
}

#[cfg(feature = "semantic")]
fn retrieve(
    store: &Store,
    query: &Query,
    directory: &Path,
    vector: &Embedding,
    hits: &mut Vec<Hit>,
    coverage: &mut serde_json::Value,
) -> Result<bool> {
    use crate::semantic::{
        SemanticHit,
        embedding_cache::{EmbeddingCache, content_key},
    };
    use std::collections::BinaryHeap;
    const FILE_LIMIT: usize = 1000;
    let cache = EmbeddingCache::open_query(directory)?;
    let vectors = cache.begin_snapshot()?;
    let mut statement = store.conn.prepare(
        "SELECT r.id,r.body,r.file_id FROM regions r JOIN files f ON f.id=r.file_id
         WHERE f.revision IS NOT NULL AND substr(f.path,1,length(?1))=?1
         AND (?2='' OR f.language=?2) AND (?3='' OR r.kind=?3) ORDER BY r.file_id,r.id",
    )?;
    let mut rows = statement.query(rusqlite::params![query.path, query.language, query.kind])?;
    let mut heap = BinaryHeap::with_capacity(FILE_LIMIT + 1);
    let mut previous_file = None;
    let mut best: Option<SemanticHit> = None;
    let mut embedded = 0_u64;
    let mut pending = 0_u64;
    let mut complete_files = 0_u64;
    let mut ranked_files = 0;
    let mut missing = false;
    let retain = |heap: &mut BinaryHeap<SemanticHit>, hit: SemanticHit| {
        heap.push(hit);
        if heap.len() > FILE_LIMIT {
            heap.pop();
        }
    };
    while let Some(row) = rows.next()? {
        let id = row.get::<_, i64>(0)? as u64;
        let body: String = row.get(1)?;
        let file: i64 = row.get(2)?;
        if previous_file != Some(file) {
            if previous_file.is_some() && !missing {
                complete_files += 1;
            }
            if let Some(hit) = best.take() {
                retain(&mut heap, hit);
                ranked_files += 1;
            }
            previous_file = Some(file);
            missing = false;
        }
        if let Some(candidate) = vectors.get(&content_key(&body))? {
            embedded += 1;
            let hit = SemanticHit {
                id,
                score: vector.cosine(&candidate),
            };
            if best.is_none_or(|old| {
                hit.score > old.score || (hit.score == old.score && hit.id < old.id)
            }) {
                best = Some(hit);
            }
        } else {
            pending += 1;
            missing = true;
        }
    }
    if previous_file.is_some() && !missing {
        complete_files += 1;
    }
    if let Some(hit) = best {
        retain(&mut heap, hit);
        ranked_files += 1;
    }
    let mut ranked = heap.into_vec();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    let mut statement = store.conn.prepare(
        "SELECT f.path,f.revision,r.start,r.end,r.name,r.kind,NULL,'semantic',c.bytes
         FROM regions r JOIN files f ON f.id=r.file_id JOIN contents c ON c.revision=f.revision WHERE r.id=?1")?;
    hits.reserve(ranked.len());
    for hit in ranked {
        hits.push(statement.query_row([hit.id as i64], super::hit_row)?);
    }
    coverage["semantic_files"] = complete_files.into();
    coverage["semantic_total_regions"] = (embedded + pending).into();
    coverage["semantic_regions"] = embedded.into();
    coverage["semantic_pending"] = pending.into();
    coverage["semantic_failures"] = vectors.status().corrupt_misses.into();
    coverage["semantic_scope"] = "query_filters".into();
    coverage["semantic_status"] = if pending == 0 { "ready" } else { "partial" }.into();
    Ok(ranked_files > FILE_LIMIT)
}

#[cfg(not(feature = "semantic"))]
fn retrieve(
    _: &Store,
    _: &Query,
    _: &Path,
    _: &Embedding,
    _: &mut Vec<Hit>,
    _: &mut serde_json::Value,
) -> Result<bool> {
    Err(crate::semantic::unavailable())
}
