//! Request-owned observations of executed retrieval lanes, before result fusion.
use crate::{results::Hit, store::Coverage};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Instant};

const MAX_LANE_IDENTITIES: usize = 128;
const MAX_IDENTITY_BYTES: usize = 24 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    ExactIdentifier,
    Lexical,
    File,
    LiveRegex,
    Semantic,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneOutcome {
    Complete,
    Partial,
    Failed,
}

impl LaneOutcome {
    pub(super) fn from_result<T>(result: &anyhow::Result<T>, partial: bool) -> Self {
        if result.is_err() {
            Self::Failed
        } else if partial {
            Self::Partial
        } else {
            Self::Complete
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RankedSourceIdentity {
    pub original_rank: usize,
    pub path: String,
    pub revision: Option<String>,
    pub start: usize,
    pub end: usize,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LaneEvent {
    pub lane: Lane,
    pub elapsed_us: u64,
    pub outcome: LaneOutcome,
    pub candidate_count: usize,
    pub result_limit_reached: bool,
    pub candidates: Vec<RankedSourceIdentity>,
    pub candidate_records_omitted: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RetrievalCoverage {
    pub index: Coverage,
    pub live_checked_files: Option<u64>,
    pub live_read_failures: Option<u64>,
    pub live_walk_failures: Option<u64>,
    pub semantic_total_regions: Option<u64>,
    pub semantic_regions: Option<u64>,
    pub semantic_failures: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RetrievalTrace {
    #[serde(skip)]
    enabled: bool,
    #[serde(skip)]
    retained_identity_bytes: usize,
    pub root_identity: String,
    pub generation: Option<i64>,
    pub query_preparation_us: Option<u64>,
    pub coverage: Option<RetrievalCoverage>,
    pub lanes: Vec<LaneEvent>,
}

impl Default for RetrievalTrace {
    fn default() -> Self {
        Self {
            enabled: true,
            root_identity: String::new(),
            retained_identity_bytes: 0,
            generation: None,
            query_preparation_us: None,
            coverage: None,
            lanes: Vec::with_capacity(5),
        }
    }
}

impl RetrievalTrace {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            lanes: Vec::new(),
            root_identity: String::new(),
            retained_identity_bytes: 0,
            generation: None,
            query_preparation_us: None,
            coverage: None,
        }
    }

    pub(super) fn begin(&mut self, root: &Path) {
        self.lanes.clear();
        self.retained_identity_bytes = 0;
        self.generation = None;
        self.query_preparation_us = None;
        self.coverage = None;
        if self.enabled {
            self.root_identity = blake3::hash(crate::store::encode_path(root).as_bytes())
                .to_hex()
                .to_string();
        }
    }

    pub(super) fn snapshot(&mut self, generation: i64, coverage: &serde_json::Value) {
        if !self.enabled {
            return;
        }
        self.generation = Some(generation);
        self.coverage =
            serde_json::from_value(coverage.clone())
                .ok()
                .map(|index| RetrievalCoverage {
                    index,
                    live_checked_files: coverage["live_checked_files"].as_u64(),
                    live_read_failures: coverage["live_read_failures"].as_u64(),
                    live_walk_failures: coverage["live_walk_failures"].as_u64(),
                    semantic_total_regions: coverage["semantic_total_regions"].as_u64(),
                    semantic_regions: coverage["semantic_regions"].as_u64(),
                    semantic_failures: coverage["semantic_failures"].as_u64(),
                });
    }

    pub(super) fn record(
        &mut self,
        lane: Lane,
        started: Instant,
        hits: &[Hit],
        outcome: LaneOutcome,
        result_limit_reached: bool,
    ) {
        if !self.enabled {
            return;
        }
        let elapsed_us = elapsed_us(started);
        let mut candidates = Vec::with_capacity(hits.len().min(MAX_LANE_IDENTITIES));
        for (i, hit) in hits.iter().take(MAX_LANE_IDENTITIES).enumerate() {
            let identity = RankedSourceIdentity {
                original_rank: i + 1,
                path: hit.path.clone(),
                revision: hit.revision.clone(),
                start: hit.start,
                end: hit.end,
                kind: hit.kind.clone(),
            };
            let bytes = serde_json::to_vec(&identity)
                .map(|bytes| bytes.len() + 1)
                .unwrap_or(usize::MAX);
            if bytes > MAX_IDENTITY_BYTES.saturating_sub(self.retained_identity_bytes) {
                break;
            }
            self.retained_identity_bytes += bytes;
            candidates.push(identity);
        }
        self.lanes.push(LaneEvent {
            lane,
            elapsed_us,
            outcome,
            candidate_count: hits.len(),
            result_limit_reached,
            candidate_records_omitted: hits.len() - candidates.len(),
            candidates,
        });
    }
}

pub(super) fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests;
