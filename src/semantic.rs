//! Pinned embeddings, shared inference, and bounded exact vector ranking.
//! Inputs contain source text only; callers join cached vectors to their read snapshot.

use anyhow::{Result, bail};
#[cfg(feature = "semantic")]
use std::cmp::Ordering;

mod content_identity;
pub(crate) use content_identity::content_key;
#[cfg(feature = "semantic")]
pub(crate) mod embedding_cache;
#[cfg(feature = "semantic")]
mod inference;
#[cfg(feature = "semantic")]
pub use inference::{
    RerankInferenceEngine, SemanticInferenceEngine, SemanticProviderConfig, run_gate, run_gpu_gate,
};

pub mod runtime_config;
pub mod worker;

mod session;
pub use session::{ResidencySample, SemanticSession};
mod verified_assets;
pub use verified_assets::VerifiedAssetDigests;

pub const MODEL_REVISION: &str = "516f4baf13dec4ddddda8631e019b5737c8bc250";
pub const DIMENSIONS: usize = 768;
#[cfg(feature = "semantic")]
pub const CACHE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
pub const MODEL_NAME: &str = "jinaai/jina-embeddings-v2-base-code";
pub const INPUT_VERSION: &str = "source-only-mean-l2-v2-regions4096-max4096";

pub const RERANKER_NAME: &str = "rozgo/bge-reranker-v2-m3";
pub const RERANKER_REVISION: &str = "fbd57b17b4db111a9d16813bb08b4c804fac18e9";

/// Maximum documents scored in one rerank request.
pub const RERANK_MAX_DOCUMENTS: usize = 32;
/// Maximum bytes accepted for the rerank query and for each document.
pub const RERANK_MAX_DOCUMENT_BYTES: usize = 4096;
/// Maximum tokenized (query, document) pair length; tokenizer truncation stays on.
#[cfg(feature = "semantic")]
pub const RERANK_MAX_TOKENS: usize = 1024;
#[cfg(feature = "semantic")]
/// Length-sorted documents per reranker batch; each batch pads to its longest
/// pair, and larger batches measured slower with higher peak memory.
pub const RERANK_BATCH_SIZE: usize = 8;

/// Validates rerank admission bounds shared with the worker protocol. Defined
/// unconditionally so the worker protocol can enforce it without the
/// `semantic` feature.
pub fn check_rerank_bounds(query: &str, documents: &[impl AsRef<str>]) -> Result<()> {
    if documents.len() > RERANK_MAX_DOCUMENTS {
        bail!(coded!(
            RerankAdmission,
            "{} documents exceeds the limit of {RERANK_MAX_DOCUMENTS}",
            documents.len()
        ));
    }
    if query.len() > RERANK_MAX_DOCUMENT_BYTES {
        bail!(coded!(
            RerankAdmission,
            "query of {} bytes exceeds the limit of {RERANK_MAX_DOCUMENT_BYTES}",
            query.len()
        ));
    }
    for (index, document) in documents.iter().enumerate() {
        let document = document.as_ref();
        if document.len() > RERANK_MAX_DOCUMENT_BYTES {
            bail!(coded!(
                RerankAdmission,
                "document at index {index} is {} bytes, exceeding the limit of {RERANK_MAX_DOCUMENT_BYTES}",
                document.len()
            ));
        }
    }
    Ok(())
}

static MODEL_INITIALIZATIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Counts actual process-local model loads, including explicit semantic checks.
pub fn model_initializations() -> u64 {
    MODEL_INITIALIZATIONS.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Clone, Debug)]
pub struct Embedding(pub [f32; DIMENSIONS]);

impl Embedding {
    pub fn from_values(values: &[f32]) -> Result<Self> {
        if values.len() != DIMENSIONS || values.iter().any(|v| !v.is_finite()) {
            bail!(coded!(InvalidEmbedding, "expected 768 finite f32 values"));
        }
        let norm = values
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt();
        if norm <= f64::EPSILON {
            bail!(coded!(InvalidEmbedding, "zero vector"));
        }
        let mut result = [0.0; DIMENSIONS];
        for (out, value) in result.iter_mut().zip(values) {
            *out = (f64::from(*value) / norm) as f32;
        }
        Ok(Self(result))
    }

    #[cfg(feature = "semantic")]
    pub fn cosine(&self, other: &Self) -> f32 {
        self.0.iter().zip(other.0.iter()).map(|(a, b)| a * b).sum()
    }
}

#[cfg(feature = "semantic")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SemanticHit {
    pub id: u64,
    pub score: f32,
}
#[cfg(feature = "semantic")]
impl Eq for SemanticHit {}
#[cfg(feature = "semantic")]
impl Ord for SemanticHit {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.id.cmp(&other.id))
    }
}
#[cfg(feature = "semantic")]
impl PartialOrd for SemanticHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(not(feature = "semantic"))]
pub fn unavailable() -> anyhow::Error {
    anyhow::anyhow!(coded!(
        SemanticUnavailable,
        "build with --features semantic-cuda (or semantic for CPU), install the pinned assets in evaluation/semantic_gate/model.json, configure inference.toml, then run semantic prepare"
    ))
}

#[cfg(not(feature = "semantic"))]
pub fn run_gate(_: &std::path::Path) -> Result<serde_json::Value> {
    Err(unavailable())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "semantic"))]
    #[test]
    fn semantic_disabled_is_actionable() {
        let error = run_gate(std::path::Path::new("unused")).unwrap_err();
        assert!(error.to_string().contains("semantic_unavailable"));
        assert!(error.to_string().contains("--features semantic"));
    }

    #[test]
    fn semantic_embedding_rejects_invalid_values() {
        assert!(Embedding::from_values(&[0.0; DIMENSIONS]).is_err());
        assert!(Embedding::from_values(&[f32::NAN; DIMENSIONS]).is_err());
        assert!(Embedding::from_values(&[1.0; 8]).is_err());
    }
}
