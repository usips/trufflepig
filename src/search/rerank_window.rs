//! Cross-encoder reranking narrows fused hit order within a bounded window.
//! Bodies are read from the current snapshot; a missing body, an unreadable
//! snapshot, or a non-finite score sinks that hit below the window's scored
//! hits without dropping it or aborting the surrounding search.
use super::telemetry::{Lane, LaneOutcome, RetrievalTrace};
use crate::{results::Hit, store::Store};
use anyhow::Result;
use rusqlite::OptionalExtension;
use std::{collections::HashSet, path::Path, time::Instant};

#[cfg(test)]
mod tests;

/// Fused hits eligible for cross-encoder scoring, from the top of the fused order.
pub(super) const RERANK_WINDOW: usize = crate::semantic::RERANK_MAX_DOCUMENTS;

/// Scores (query, document) pairs; implemented by `SemanticSession` over the
/// shared worker and by tests with fakes that never contact inference.
pub trait RerankScorer {
    fn score(&self, cache_identity: &Path, query: &str, documents: &[String]) -> Result<Vec<f32>>;
}

impl RerankScorer for crate::semantic::SemanticSession {
    fn score(&self, cache_identity: &Path, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        self.rerank(cache_identity, query, documents)
    }
}

const BODY_CLAMP_BYTES: usize = crate::semantic::RERANK_MAX_DOCUMENT_BYTES;

pub(super) fn apply(
    store: &Store,
    query_text: &str,
    cache: &Path,
    scorer: &dyn RerankScorer,
    hits: &mut [Hit],
    coverage: &mut serde_json::Value,
    trace: &mut RetrievalTrace,
) {
    let started = Instant::now();
    if query_text.is_empty() || hits.is_empty() {
        coverage["rerank_status"] = "skipped".into();
        trace.record(Lane::Rerank, started, &[], LaneOutcome::Complete, false);
        return;
    }
    let window = hits.len().min(RERANK_WINDOW);
    let (documents, scored_positions) = match fetch_window_bodies(store, hits, window) {
        Ok(fetched) => fetched,
        Err(error) => {
            coverage["rerank_status"] = "unavailable".into();
            coverage["rerank_reason"] = format!("{error:#}").into();
            trace.record(Lane::Rerank, started, &[], LaneOutcome::Failed, false);
            return;
        }
    };
    if documents.is_empty() {
        coverage["rerank_status"] = "skipped".into();
        trace.record(Lane::Rerank, started, &[], LaneOutcome::Complete, false);
        return;
    }
    match scorer.score(cache, query_text, &documents) {
        Ok(scores) if scores.len() != documents.len() => {
            coverage["rerank_status"] = "unavailable".into();
            coverage["rerank_reason"] = format!(
                "rerank_worker: score count mismatch: {} scores for {} documents",
                scores.len(),
                documents.len()
            )
            .into();
            trace.record(Lane::Rerank, started, &[], LaneOutcome::Failed, false);
        }
        Ok(scores) => {
            let mut scored: Vec<(usize, f32)> = scored_positions
                .iter()
                .zip(scores.iter())
                .filter(|(_, score)| score.is_finite())
                .map(|(&position, &score)| (position, score))
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let kept: HashSet<usize> = scored.iter().map(|(position, _)| *position).collect();
            let mut sources: Vec<usize> = scored.iter().map(|(position, _)| *position).collect();
            sources.extend((0..window).filter(|position| !kept.contains(position)));
            // `sources[target]` is the window position that belongs at `target`;
            // invert it into a swap-destination map before applying in place.
            let mut destinations = vec![0; window];
            for (target, &source) in sources.iter().enumerate() {
                destinations[source] = target;
            }
            permute_in_place(&mut hits[..window], &mut destinations);
            coverage["rerank_status"] = "ready".into();
            coverage["rerank_window"] = scored.len().into();
            trace.record(
                Lane::Rerank,
                started,
                &hits[..window],
                LaneOutcome::Complete,
                false,
            );
        }
        Err(error) => {
            coverage["rerank_status"] = "unavailable".into();
            coverage["rerank_reason"] = format!("{error:#}").into();
            trace.record(Lane::Rerank, started, &[], LaneOutcome::Failed, false);
        }
    }
}

/// Reads bodies for the window's fused hits. A hit without a readable
/// current body (missing content row, or a clamp that yields no text) is
/// omitted from the result but does not fail the fetch; only a snapshot
/// read error does, so a broken statement or row cannot abort the search.
fn fetch_window_bodies(
    store: &Store,
    hits: &[Hit],
    window: usize,
) -> Result<(Vec<String>, Vec<usize>)> {
    let mut statement = store.conn.prepare(
        "SELECT substr(c.bytes, ?2 + 1, ?3 - ?2) FROM files f JOIN contents c
         ON c.revision = f.revision WHERE f.path = ?1",
    )?;
    let mut documents: Vec<String> = Vec::with_capacity(window);
    let mut scored_positions: Vec<usize> = Vec::with_capacity(window);
    for (position, hit) in hits[..window].iter().enumerate() {
        let (start, end) = if hit.start == hit.end {
            (0_i64, BODY_CLAMP_BYTES as i64)
        } else {
            (hit.start as i64, hit.end as i64)
        };
        let body: Option<Vec<u8>> = statement
            .query_row(rusqlite::params![hit.path, start, end], |row| row.get(0))
            .optional()?;
        let Some(bytes) = body else { continue };
        let Some(text) = clamp_document(&bytes) else {
            continue;
        };
        documents.push(text);
        scored_positions.push(position);
    }
    Ok((documents, scored_positions))
}

/// Clamps a body to `BODY_CLAMP_BYTES` on a UTF-8 char boundary. An empty
/// clamp (including a clamp that lands mid-way through the first character)
/// yields `None`, so the caller leaves that hit unscored.
pub(super) fn clamp_document(bytes: &[u8]) -> Option<String> {
    let clamped = &bytes[..bytes.len().min(BODY_CLAMP_BYTES)];
    let end = std::str::from_utf8(clamped)
        .err()
        .map(|error| error.valid_up_to())
        .unwrap_or(clamped.len());
    if end == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&clamped[..end]).into_owned())
}

/// Moves `slice[i]` to `destinations[i]` for every `i`, in place, by
/// following permutation cycles with swaps instead of cloning elements.
/// `destinations` is consumed as scratch space.
fn permute_in_place<T>(slice: &mut [T], destinations: &mut [usize]) {
    debug_assert_eq!(slice.len(), destinations.len());
    for i in 0..destinations.len() {
        while destinations[i] != i {
            let j = destinations[i];
            slice.swap(i, j);
            destinations.swap(i, j);
        }
    }
}
