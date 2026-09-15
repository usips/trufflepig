//! Cross-encoder reranking of a small candidate window.
//! Bounds admission before tokenizing; scores return in caller order.

use super::{RERANK_BATCH_SIZE, SemanticProviderConfig, check_rerank_bounds, provider};
use anyhow::{Result, bail};
use fastembed::TextRerank;
use std::path::Path;

/// A model-only reranking engine for workers that own their model lifetime.
pub struct RerankInferenceEngine {
    model: TextRerank,
}

impl RerankInferenceEngine {
    /// Opens the pinned reranker using `provider`'s configured execution provider.
    pub fn open_with_provider(model_dir: &Path, provider: SemanticProviderConfig) -> Result<Self> {
        let model = provider::load_verified_reranker(model_dir, provider)?;
        Ok(Self { model })
    }

    /// Opens the pinned reranker using an explicit ONNX Runtime shared library path.
    pub fn open_with_runtime(
        model_dir: &Path,
        provider: SemanticProviderConfig,
        runtime_library: &Path,
    ) -> Result<Self> {
        let model = self::provider::load_verified_reranker_with_runtime(
            model_dir,
            provider,
            runtime_library,
        )?;
        Ok(Self { model })
    }

    /// Scores each document against `query`, one score per document, in
    /// caller order. Rejects requests exceeding
    /// [`RERANK_MAX_DOCUMENTS`](crate::semantic::RERANK_MAX_DOCUMENTS) or
    /// [`RERANK_MAX_DOCUMENT_BYTES`](crate::semantic::RERANK_MAX_DOCUMENT_BYTES)
    /// before running the model.
    pub fn score(&mut self, query: &str, documents: &[&str]) -> Result<Vec<f32>> {
        check_rerank_bounds(query, documents)?;
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let results = self
            .model
            .rerank(query, documents, false, Some(RERANK_BATCH_SIZE))?;
        scores_in_caller_order(
            results.into_iter().map(|r| (r.index, r.score)).collect(),
            documents.len(),
        )
    }
}

/// Reassembles fastembed's score-sorted results into caller order, failing
/// closed on a missing or duplicated index. A non-finite score passes
/// through unchanged; the search window sinks it rather than the request
/// failing outright.
fn scores_in_caller_order(results: Vec<(usize, f32)>, count: usize) -> Result<Vec<f32>> {
    let mut scores: Vec<Option<f32>> = vec![None; count];
    for (index, score) in results {
        let slot = scores.get_mut(index).ok_or_else(|| {
            anyhow::anyhow!("rerank_unavailable: result index {index} out of range")
        })?;
        if slot.is_some() {
            bail!("rerank_unavailable: duplicate result index {index}");
        }
        *slot = Some(score);
    }
    scores
        .into_iter()
        .enumerate()
        .map(|(index, score)| {
            score.ok_or_else(|| anyhow::anyhow!("rerank_unavailable: missing result index {index}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::{RERANK_MAX_DOCUMENT_BYTES, RERANK_MAX_DOCUMENTS};

    #[test]
    fn scores_reorder_into_caller_order() {
        let results = vec![(2, 0.1), (0, 0.9), (1, 0.5)];
        let scores = scores_in_caller_order(results, 3).unwrap();
        assert_eq!(scores, vec![0.9, 0.5, 0.1]);
    }

    #[test]
    fn scores_reject_missing_index() {
        let results = vec![(0, 0.9)];
        let error = scores_in_caller_order(results, 2).unwrap_err();
        assert!(error.to_string().contains("missing result index 1"));
    }

    #[test]
    fn scores_reject_duplicate_index() {
        let results = vec![(0, 0.9), (0, 0.1)];
        let error = scores_in_caller_order(results, 1).unwrap_err();
        assert!(error.to_string().contains("duplicate result index 0"));
    }

    #[test]
    fn scores_pass_through_non_finite() {
        let results = vec![(0, f32::NAN)];
        let scores = scores_in_caller_order(results, 1).unwrap();
        assert!(scores[0].is_nan());
    }

    #[test]
    fn bounds_reject_too_many_documents() {
        let documents = vec!["doc"; RERANK_MAX_DOCUMENTS + 1];
        let error = check_rerank_bounds("query", &documents).unwrap_err();
        assert!(error.to_string().contains("rerank_admission"));
    }

    #[test]
    fn bounds_reject_oversized_query_and_document() {
        let long = "a".repeat(RERANK_MAX_DOCUMENT_BYTES + 1);
        assert!(
            check_rerank_bounds(&long, &["doc"])
                .unwrap_err()
                .to_string()
                .contains("query")
        );
        let error = check_rerank_bounds("query", &[long.as_str()]).unwrap_err();
        assert!(error.to_string().contains("document at index 0"));
    }

    #[test]
    fn bounds_accept_empty_documents() {
        let documents: Vec<&str> = Vec::new();
        assert!(check_rerank_bounds("query", &documents).is_ok());
    }
}
