//! Cross-encoder reranking of a small candidate window.
//! Bounds admission before tokenizing; scores return in caller order.

use super::{RERANK_BATCH_SIZE, SemanticProviderConfig, check_rerank_bounds, provider};
use crate::VerifiedAssetDigests;
use anyhow::{Result, bail};
use fastembed::TextRerank;
use std::path::Path;

/// A model-only reranking engine for workers that own their model lifetime.
pub struct RerankInferenceEngine {
    model: TextRerank,
}

impl RerankInferenceEngine {
    /// Opens the pinned reranker using `provider`'s configured execution provider.
    /// `digests` skips re-hashing assets its owner already verified.
    pub fn open_with_provider(
        model_dir: &Path,
        provider: SemanticProviderConfig,
        digests: &mut VerifiedAssetDigests,
    ) -> Result<Self> {
        let model = provider::load_verified_reranker(model_dir, provider, digests)?;
        Ok(Self { model })
    }

    /// Opens the pinned reranker using an explicit ONNX Runtime shared library path.
    pub fn open_with_runtime(
        model_dir: &Path,
        provider: SemanticProviderConfig,
        runtime_library: &Path,
        digests: &mut VerifiedAssetDigests,
    ) -> Result<Self> {
        let model = self::provider::load_verified_reranker_with_runtime(
            model_dir,
            provider,
            runtime_library,
            digests,
        )?;
        Ok(Self { model })
    }

    /// Scores each document against `query`, one score per document, in
    /// caller order. Rejects requests exceeding
    /// [`RERANK_MAX_DOCUMENTS`](crate::RERANK_MAX_DOCUMENTS) or
    /// [`RERANK_MAX_DOCUMENT_BYTES`](crate::RERANK_MAX_DOCUMENT_BYTES)
    /// before running the model.
    pub fn score(&mut self, query: &str, documents: &[&str]) -> Result<Vec<f32>> {
        check_rerank_bounds(query, documents)?;
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        // fastembed pads each batch to its longest pair, so length-sorted
        // batches spend far less compute on padding than fused order does.
        let order = length_sorted_order(documents);
        let sorted = order
            .iter()
            .map(|&index| documents[index])
            .collect::<Vec<_>>();
        let results = self
            .model
            .rerank(query, &sorted, false, Some(RERANK_BATCH_SIZE))?;
        scores_in_caller_order(
            sorted_results_to_caller(&order, results.into_iter().map(|r| (r.index, r.score)))?,
            documents.len(),
        )
    }
}

/// Caller indices ordered by document byte length (a token-length proxy, as
/// the query is shared), ties keeping caller order.
fn length_sorted_order(documents: &[&str]) -> Vec<usize> {
    let mut order = (0..documents.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| documents[index].len());
    order
}

/// Maps results indexed by sorted position back to caller indices, failing
/// closed on a position outside `order`.
fn sorted_results_to_caller(
    order: &[usize],
    results: impl Iterator<Item = (usize, f32)>,
) -> Result<Vec<(usize, f32)>> {
    results
        .map(|(position, score)| {
            let index = order.get(position).ok_or_else(|| {
                anyhow::anyhow!(coded!(
                    RerankUnavailable,
                    "result index {position} out of range"
                ))
            })?;
            Ok((*index, score))
        })
        .collect()
}

/// Reassembles fastembed's score-sorted results into caller order, failing
/// closed on a missing or duplicated index. A non-finite score passes
/// through unchanged; the search window sinks it rather than the request
/// failing outright.
fn scores_in_caller_order(results: Vec<(usize, f32)>, count: usize) -> Result<Vec<f32>> {
    let mut scores: Vec<Option<f32>> = vec![None; count];
    for (index, score) in results {
        let slot = scores.get_mut(index).ok_or_else(|| {
            anyhow::anyhow!(coded!(
                RerankUnavailable,
                "result index {index} out of range"
            ))
        })?;
        if slot.is_some() {
            bail!(coded!(RerankUnavailable, "duplicate result index {index}"));
        }
        *slot = Some(score);
    }
    scores
        .into_iter()
        .enumerate()
        .map(|(index, score)| {
            score.ok_or_else(|| {
                anyhow::anyhow!(coded!(RerankUnavailable, "missing result index {index}"))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RERANK_MAX_DOCUMENT_BYTES, RERANK_MAX_DOCUMENTS};

    #[test]
    fn scores_reorder_into_caller_order() {
        let results = vec![(2, 0.1), (0, 0.9), (1, 0.5)];
        let scores = scores_in_caller_order(results, 3).unwrap();
        assert_eq!(scores, vec![0.9, 0.5, 0.1]);
    }

    #[test]
    fn length_sorted_scores_map_back_to_caller_order() {
        let documents = ["ccc", "a", "bbbb", "dd", "e"];
        let order = length_sorted_order(&documents);
        assert_eq!(order, vec![1, 4, 3, 0, 2]);
        // Score each sorted document by its text so the remap is checkable,
        // returned in fastembed's descending-score order.
        let score_of = |text: &str| text.len() as f32 * 10.0 + f32::from(text.as_bytes()[0]);
        let mut results = order
            .iter()
            .enumerate()
            .map(|(position, &index)| (position, score_of(documents[index])))
            .collect::<Vec<_>>();
        results.sort_by(|a, b| b.1.total_cmp(&a.1));
        let caller = sorted_results_to_caller(&order, results.into_iter()).unwrap();
        let scores = scores_in_caller_order(caller, documents.len()).unwrap();
        let expected = documents
            .iter()
            .map(|text| score_of(text))
            .collect::<Vec<_>>();
        assert_eq!(scores, expected);
    }

    #[test]
    fn sorted_results_reject_out_of_range_position() {
        let error = sorted_results_to_caller(&[0, 1], [(2, 0.5)].into_iter()).unwrap_err();
        assert!(error.to_string().contains("result index 2 out of range"));
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
