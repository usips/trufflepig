//! Model loading, provider selection, bounded uncached inference, and gates.
//! The inference worker owns the model independently of repository caches.

mod batch;
mod gate;
mod provider;
mod reranker;

pub use gate::{compare_cpu_cuda, run_gate, run_gpu_gate};
pub use reranker::RerankInferenceEngine;
// Bounds and their validator live in `crate::semantic` unconditionally so the
// worker protocol can enforce them without the `semantic` feature; re-export
// them here so this module's siblings (e.g. `provider`, `reranker`) can keep
// referring to them as `super::NAME`.
pub use super::{RERANK_BATCH_SIZE, RERANK_MAX_TOKENS, check_rerank_bounds};

use super::Embedding;
use anyhow::Result;
use fastembed::TextEmbedding;
use std::path::Path;

/// Maximum number of source texts submitted to one fastembed call.
pub const MAX_BATCH_INPUTS: usize = 8;
/// Maximum padded token count submitted to one fastembed call.
pub const MAX_BATCH_PADDED_TOKENS: usize = 8192;
/// Maximum unpadded token count accepted for one source text.
pub const MAX_INPUT_TOKENS: usize = 4096;
/// CUDA arena limit used by [`SemanticProviderConfig::cuda`].
pub const DEFAULT_CUDA_ARENA_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// Minimum per-input cosine for the CPU/CUDA cached-bypass check.
pub const CPU_CUDA_PARITY_COSINE: f32 = 0.999;

/// Selects the execution provider for a semantic model session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticProvider {
    /// Run the ONNX graph with ONNX Runtime's CPU provider.
    Cpu,
    /// Run the graph on one CUDA device, allowing unsupported auxiliary nodes on CPU.
    Cuda { device_ordinal: i32 },
}

/// Runtime settings shared by CPU and CUDA semantic sessions.
///
/// CUDA always disables TF32 and fails closed if provider registration fails;
/// unsupported auxiliary graph nodes may still execute on CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticProviderConfig {
    pub provider: SemanticProvider,
    pub arena_max_bytes: u64,
}

impl Default for SemanticProviderConfig {
    fn default() -> Self {
        Self::cpu()
    }
}

impl SemanticProviderConfig {
    /// Creates a CPU provider configuration.
    pub const fn cpu() -> Self {
        Self {
            provider: SemanticProvider::Cpu,
            arena_max_bytes: DEFAULT_CUDA_ARENA_MAX_BYTES,
        }
    }

    /// Creates a CUDA configuration for a device ordinal.
    pub const fn cuda(device_ordinal: i32) -> Self {
        Self {
            provider: SemanticProvider::Cuda { device_ordinal },
            arena_max_bytes: DEFAULT_CUDA_ARENA_MAX_BYTES,
        }
    }

    /// Sets the CUDA execution provider arena limit in bytes.
    #[must_use]
    pub const fn with_arena_max_bytes(mut self, bytes: u64) -> Self {
        self.arena_max_bytes = bytes;
        self
    }

    /// Validates provider values before model or runtime initialization.
    pub fn validate(&self) -> Result<()> {
        if self.arena_max_bytes == 0 {
            anyhow::bail!("semantic_unavailable: provider arena limit must be positive");
        }
        if let SemanticProvider::Cuda { device_ordinal } = self.provider {
            if device_ordinal < 0 {
                anyhow::bail!(
                    "semantic_unavailable: CUDA device ordinal must be non-negative, got {device_ordinal}"
                );
            }
        }
        Ok(())
    }
}

/// A model-only semantic engine for workers that own their model lifetime.
pub struct SemanticInferenceEngine {
    model: TextEmbedding,
    publisher_parity_cosine: f32,
}

impl SemanticInferenceEngine {
    /// Opens the pinned model with the CPU provider and no embedding cache.
    pub fn open(model_dir: &Path) -> Result<Self> {
        Self::open_with_provider(model_dir, SemanticProviderConfig::cpu())
    }

    /// Opens the pinned model with an explicit execution provider and no cache.
    pub fn open_with_provider(model_dir: &Path, provider: SemanticProviderConfig) -> Result<Self> {
        let mut model = provider::load_verified_model(model_dir, provider)?;
        let publisher_parity_cosine = gate::check_parity(&mut model)?;
        Ok(Self {
            model,
            publisher_parity_cosine,
        })
    }

    /// Opens a model using an explicit ONNX Runtime shared library path.
    ///
    /// This avoids changing the process environment, which lets a worker
    /// select its runtime before other threads perform inference.
    pub fn open_with_runtime(
        model_dir: &Path,
        provider: SemanticProviderConfig,
        runtime_library: &Path,
    ) -> Result<Self> {
        let mut model =
            provider::load_verified_model_with_runtime(model_dir, provider, runtime_library)?;
        let publisher_parity_cosine = gate::check_parity(&mut model)?;
        Ok(Self {
            model,
            publisher_parity_cosine,
        })
    }

    /// Runs one uncached model inference.
    pub fn embed_uncached(&mut self, text: &str) -> Result<Embedding> {
        batch::infer(&mut self.model, text)
    }

    /// Returns the measured cosine from the publisher's frozen two-input gate.
    pub fn publisher_parity_cosine(&self) -> f32 {
        self.publisher_parity_cosine
    }

    /// Runs bounded uncached model inference.
    pub fn embed_batch_uncached<S: AsRef<str> + Send + Sync>(
        &mut self,
        texts: &[S],
    ) -> Result<Vec<Embedding>> {
        let texts = texts.iter().map(|text| text.as_ref()).collect::<Vec<_>>();
        batch::infer_batch(&mut self.model, &texts)
    }

    /// Runs bounded inference while isolating failures to individual inputs.
    pub fn embed_batch_uncached_isolated<S: AsRef<str> + Send + Sync>(
        &mut self,
        texts: &[S],
    ) -> Vec<Result<Embedding>> {
        let texts = texts.iter().map(|text| text.as_ref()).collect::<Vec<_>>();
        batch::infer_batch_isolated(&mut self.model, &texts)
    }
}

#[cfg(test)]
mod tests;
