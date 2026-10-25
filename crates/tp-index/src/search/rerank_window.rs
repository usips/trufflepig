//! Cross-encoder reranking narrows fused hit order within a bounded window.
//! Phrase hits, and an identifier query's evidence hits, stay ahead of other
//! hits; scores order each tier, and fused position breaks ties.
//! Bodies come from the indexed content of each hit's revision; a missing body
//! or a non-finite score sinks that hit below its tier's scored hits without
//! dropping it. A snapshot read error or scorer failure keeps the fused order.
use super::{
    Query,
    concept_query::ConceptQuery,
    file_ranking::{FileEvidence, FusionPolicy},
    path_prior::PathPrior,
    telemetry::{Lane, LaneOutcome, RetrievalTrace},
};
use crate::{results::Hit, store::Store};
use anyhow::Result;
use rusqlite::OptionalExtension;
use std::{
    path::Path,
    time::{Duration, Instant},
};
use tp_core::query_deadline::QueryDeadline;

#[cfg(test)]
mod tests;

/// Fused hits eligible for cross-encoder scoring, from the top of the fused order.
pub const RERANK_WINDOW: usize = tp_semantic::RERANK_MAX_DOCUMENTS;

/// Scores (query, document) pairs within `budget` (capped by the worker's own
/// deadline); implemented by `SemanticSession` over the shared worker and by
/// tests with fakes that never contact inference.
pub trait RerankScorer {
    fn score(
        &self,
        cache_identity: &Path,
        query: &str,
        documents: &[String],
        budget: Duration,
    ) -> Result<Vec<f32>>;
}

impl RerankScorer for tp_semantic::SemanticSession {
    fn score(
        &self,
        cache_identity: &Path,
        query: &str,
        documents: &[String],
        budget: Duration,
    ) -> Result<Vec<f32>> {
        self.rerank(cache_identity, query, documents, budget)
    }
}

const BODY_CLAMP_BYTES: usize = tp_semantic::RERANK_MAX_DOCUMENT_BYTES;

/// What one rerank stage reports as `rerank_status`, `rerank_reason`, and
/// `rerank_window` in coverage.
#[derive(Clone, Debug, PartialEq)]
pub enum RerankOutcome {
    /// Nothing to score: empty query text, no hits, or no readable bodies.
    Skipped,
    /// The fused order stands; the reason names the timeout or failure.
    Unavailable(String),
    /// The window was reordered; `scored` hits carried a finite score.
    Ready { scored: usize },
}

impl RerankOutcome {
    pub fn record(&self, coverage: &mut serde_json::Value) {
        match self {
            Self::Skipped => coverage["rerank_status"] = "skipped".into(),
            Self::Unavailable(reason) => {
                coverage["rerank_status"] = "unavailable".into();
                coverage["rerank_reason"] = reason.clone().into();
            }
            Self::Ready { scored } => {
                coverage["rerank_status"] = "ready".into();
                coverage["rerank_window"] = (*scored).into();
            }
        }
    }
}

/// Reranks one root's fused hits in place under the worker's rerank deadline.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    store: &Store,
    query_text: &str,
    policy: &FusionPolicy,
    cache: &Path,
    scorer: &dyn RerankScorer,
    hits: &mut [Hit],
    coverage: &mut serde_json::Value,
    trace: &mut RetrievalTrace,
) {
    let window = hits.len().min(RERANK_WINDOW);
    let stage = RerankStage {
        query_text,
        policy,
        cache,
        scorer,
        budget: tp_semantic::worker::RERANK_DEADLINE,
    };
    let window = &mut hits[..window];
    let (outcome, _) = stage.run(
        window,
        |hit| Some(hit),
        |hit| indexed_body(store, hit),
        trace,
    );
    outcome.record(coverage);
}

/// One rerank of the top of a ranking assembled outside one root's search (a
/// merged multi-root ranking, or a parent-index answer after its worktree
/// recheck), as a single scorer request whose budget is the query deadline's
/// remainder, capped by the worker's rerank deadline.
pub struct FinalRerank<'a> {
    pub query: &'a Query,
    /// Worker scheduling identity for the request.
    pub cache: &'a Path,
    pub scorer: &'a dyn RerankScorer,
    pub deadline: &'a QueryDeadline,
}

impl FinalRerank<'_> {
    /// Reorders the first [`RERANK_WINDOW`] items in place. `hit_of` exposes
    /// each item's hit and `store_of` the store holding its indexed body; items
    /// without either stay unscored. Returns the outcome and, per reordered
    /// window position, whether that item scored.
    pub fn run<'s, T>(
        &self,
        items: &mut [T],
        hit_of: impl Fn(&T) -> Option<&Hit>,
        store_of: impl Fn(&T) -> Option<&'s Store>,
        trace: &mut RetrievalTrace,
    ) -> (RerankOutcome, Vec<bool>) {
        let started = Instant::now();
        let text = &self.query.text;
        let budget = self
            .deadline
            .remaining()
            .min(tp_semantic::worker::RERANK_DEADLINE);
        if budget.is_zero() && !text.is_empty() && !items.is_empty() {
            trace.record(Lane::Rerank, started, &[], LaneOutcome::Failed, false);
            let reason = coded!(SemanticTimeout, "query deadline expired before rerank");
            return (RerankOutcome::Unavailable(reason.to_string()), Vec::new());
        }
        let prior = PathPrior::for_query(self.query);
        let policy = FusionPolicy {
            lane_weights: true,
            identifier_tier: ConceptQuery::parse(text).identifier.is_some(),
            prior: &prior,
            prior_from_lane: 0,
        };
        let stage = RerankStage {
            query_text: text,
            policy: &policy,
            cache: self.cache,
            scorer: self.scorer,
            budget,
        };
        let window = items.len().min(RERANK_WINDOW);
        let body_of = |item: &T| match (hit_of(item), store_of(item)) {
            (Some(hit), Some(store)) => indexed_body(store, hit),
            _ => Ok(None),
        };
        stage.run(&mut items[..window], &hit_of, body_of, trace)
    }
}

/// One scorer request over a window: query, tier policy, worker identity, budget.
struct RerankStage<'a> {
    query_text: &'a str,
    policy: &'a FusionPolicy<'a>,
    cache: &'a Path,
    scorer: &'a dyn RerankScorer,
    budget: Duration,
}

impl RerankStage<'_> {
    /// Scores the window's readable bodies and reorders it by evidence tier,
    /// then descending score, then fused position.
    fn run<T>(
        &self,
        window: &mut [T],
        hit_of: impl Fn(&T) -> Option<&Hit>,
        mut body_of: impl FnMut(&T) -> Result<Option<String>>,
        trace: &mut RetrievalTrace,
    ) -> (RerankOutcome, Vec<bool>) {
        let started = Instant::now();
        let failed = |trace: &mut RetrievalTrace, reason: String| {
            trace.record(Lane::Rerank, started, &[], LaneOutcome::Failed, false);
            (RerankOutcome::Unavailable(reason), Vec::new())
        };
        let skipped = |trace: &mut RetrievalTrace| {
            trace.record(Lane::Rerank, started, &[], LaneOutcome::Complete, false);
            (RerankOutcome::Skipped, Vec::new())
        };
        if self.query_text.is_empty() || window.is_empty() {
            return skipped(trace);
        }
        let mut documents = Vec::with_capacity(window.len());
        let mut scored_positions = Vec::with_capacity(window.len());
        for (position, item) in window.iter().enumerate() {
            match body_of(item) {
                Ok(Some(body)) => {
                    documents.push(body);
                    scored_positions.push(position);
                }
                Ok(None) => {}
                Err(error) => return failed(trace, format!("{error:#}")),
            }
        }
        if documents.is_empty() {
            return skipped(trace);
        }
        let scores = match self
            .scorer
            .score(self.cache, self.query_text, &documents, self.budget)
        {
            Ok(scores) if scores.len() != documents.len() => {
                let reason = coded!(
                    RerankWorker,
                    "score count mismatch: {} scores for {} documents",
                    scores.len(),
                    documents.len()
                );
                return failed(trace, reason.to_string());
            }
            Ok(scores) => scores,
            Err(error) => return failed(trace, format!("{error:#}")),
        };
        let mut window_scores: Vec<Option<f32>> = vec![None; window.len()];
        for (&position, &score) in scored_positions.iter().zip(scores.iter()) {
            window_scores[position] = score.is_finite().then_some(score);
        }
        let tier = |item: &T| {
            hit_of(item).map_or(u8::MAX, |hit| FileEvidence::of(hit, self.policy).tier())
        };
        let mut sources: Vec<usize> = (0..window.len()).collect();
        sources.sort_by(|&left, &right| {
            tier(&window[left])
                .cmp(&tier(&window[right]))
                .then_with(|| match (window_scores[left], window_scores[right]) {
                    (Some(left), Some(right)) => right.total_cmp(&left),
                    (left, right) => right.is_some().cmp(&left.is_some()),
                })
                .then_with(|| left.cmp(&right))
        });
        let scored: Vec<bool> = sources
            .iter()
            .map(|&source| window_scores[source].is_some())
            .collect();
        // `sources[target]` is the window position that belongs at `target`;
        // invert it into a swap-destination map before applying in place.
        let mut destinations = vec![0; window.len()];
        for (target, &source) in sources.iter().enumerate() {
            destinations[source] = target;
        }
        permute_in_place(window, &mut destinations);
        trace.record_ranked(
            Lane::Rerank,
            started,
            window.iter().filter_map(&hit_of),
            window.len(),
            LaneOutcome::Complete,
            false,
        );
        let count = scored.iter().filter(|&&scored| scored).count();
        (RerankOutcome::Ready { scored: count }, scored)
    }
}

/// Reads a hit's clamped body from the indexed content of its revision (the
/// file's indexed revision when the hit carries none). A hit without readable
/// content, including one replaced from current source, yields `None`; only a
/// snapshot read error fails.
fn indexed_body(store: &Store, hit: &Hit) -> Result<Option<String>> {
    let mut statement = store.conn.prepare_cached(
        "SELECT substr(c.bytes, ?2 + 1, ?3 - ?2) FROM contents c
         WHERE c.revision = COALESCE(?4, (SELECT revision FROM files WHERE path = ?1))",
    )?;
    let (start, end) = if hit.start == hit.end {
        (0_i64, BODY_CLAMP_BYTES as i64)
    } else {
        (hit.start as i64, hit.end as i64)
    };
    let body: Option<Vec<u8>> = statement
        .query_row(
            rusqlite::params![hit.path, start, end, hit.revision],
            |row| row.get(0),
        )
        .optional()?;
    Ok(body.as_deref().and_then(clamp_document))
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
