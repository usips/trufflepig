use super::super::{MODEL_NAME, MODEL_REVISION};
use super::{
    CPU_CUDA_PARITY_COSINE, MAX_BATCH_INPUTS, MAX_BATCH_PADDED_TOKENS, MAX_INPUT_TOKENS,
    SemanticInferenceEngine, SemanticProvider, SemanticProviderConfig, batch,
};
use anyhow::{Result, bail};
use fastembed::TextEmbedding;
use serde_json::Value;
use std::{fs, path::Path, time::Instant};

const PARITY_QUERY: &str =
    "How do I access the index while iterating over a sequence with a for loop?";
const PARITY_SOURCE: &str =
    "# Use the built-in enumerator\nfor idx, x in enumerate(xs):\n    print(idx, x)";

pub(super) fn check_parity(model: &mut TextEmbedding) -> Result<f32> {
    let query = batch::infer(model, PARITY_QUERY)?;
    let source = batch::infer(model, PARITY_SOURCE)?;
    let cosine = query.cosine(&source);
    if (cosine - 0.7281749).abs() > 0.002 {
        bail!("semantic_unavailable: publisher parity gate failed: cosine {cosine}");
    }
    Ok(cosine)
}

/// Compares cached-bypass CPU and CUDA outputs for both publisher inputs.
///
/// Each model still runs the publisher parity gate during initialization. The
/// returned cosine is the lower of the two per-input CPU/CUDA cosines.
pub fn compare_cpu_cuda(model_dir: &Path, cuda_provider: SemanticProviderConfig) -> Result<f32> {
    Ok(cpu_cuda_comparison(model_dir, cuda_provider)?.minimum_cosine)
}

struct CpuCudaComparison {
    cpu_publisher_parity: f32,
    cuda_publisher_parity: f32,
    query_cosine: f32,
    source_cosine: f32,
    minimum_cosine: f32,
    cpu_batch_query_cosine: f32,
    cpu_batch_source_cosine: f32,
    cuda_batch_query_cosine: f32,
    cuda_batch_source_cosine: f32,
}

fn cpu_cuda_comparison(
    model_dir: &Path,
    cuda_provider: SemanticProviderConfig,
) -> Result<CpuCudaComparison> {
    if !matches!(cuda_provider.provider, SemanticProvider::Cuda { .. }) {
        bail!("semantic_unavailable: CPU/CUDA comparison requires a CUDA provider");
    }
    let mut cpu =
        SemanticInferenceEngine::open_with_provider(model_dir, SemanticProviderConfig::cpu())?;
    let mut cuda = SemanticInferenceEngine::open_with_provider(model_dir, cuda_provider)?;
    let cpu_batch = cpu.embed_batch_uncached(&[PARITY_QUERY, PARITY_SOURCE])?;
    let cuda_batch = cuda.embed_batch_uncached(&[PARITY_QUERY, PARITY_SOURCE])?;
    let cpu_query = cpu.embed_uncached(PARITY_QUERY)?;
    let cpu_source = cpu.embed_uncached(PARITY_SOURCE)?;
    let cuda_query = cuda.embed_uncached(PARITY_QUERY)?;
    let cuda_source = cuda.embed_uncached(PARITY_SOURCE)?;
    let query_cosine = cpu_query.cosine(&cuda_query);
    let source_cosine = cpu_source.cosine(&cuda_source);
    let minimum_cosine = query_cosine.min(source_cosine);
    let cpu_batch_query_cosine = cpu_query.cosine(&cpu_batch[0]);
    let cpu_batch_source_cosine = cpu_source.cosine(&cpu_batch[1]);
    let cuda_batch_query_cosine = cuda_query.cosine(&cuda_batch[0]);
    let cuda_batch_source_cosine = cuda_source.cosine(&cuda_batch[1]);
    let batch_consistency = cpu_batch_query_cosine
        .min(cpu_batch_source_cosine)
        .min(cuda_batch_query_cosine)
        .min(cuda_batch_source_cosine);
    if !minimum_cosine.is_finite()
        || minimum_cosine < CPU_CUDA_PARITY_COSINE
        || !batch_consistency.is_finite()
        || batch_consistency < CPU_CUDA_PARITY_COSINE
    {
        bail!(
            "semantic_unavailable: CPU/CUDA cached-bypass parity failed: cpu_cuda_min={minimum_cosine}, batch_consistency_min={batch_consistency}, required >= {CPU_CUDA_PARITY_COSINE}"
        );
    }
    Ok(CpuCudaComparison {
        cpu_publisher_parity: cpu.publisher_parity_cosine(),
        cuda_publisher_parity: cuda.publisher_parity_cosine(),
        query_cosine,
        source_cosine,
        minimum_cosine,
        cpu_batch_query_cosine,
        cpu_batch_source_cosine,
        cuda_batch_query_cosine,
        cuda_batch_source_cosine,
    })
}

/// Executes the CPU/CUDA cached-bypass gate and returns a machine-readable report.
pub fn run_gpu_gate(model_dir: &Path, cuda_provider: SemanticProviderConfig) -> Result<Value> {
    let started = Instant::now();
    let comparison = cpu_cuda_comparison(model_dir, cuda_provider)?;
    let device_ordinal = match cuda_provider.provider {
        SemanticProvider::Cuda { device_ordinal } => device_ordinal,
        SemanticProvider::Cpu => unreachable!("compare_cpu_cuda validates the provider"),
    };
    Ok(serde_json::json!({
        "status": "passed",
        "model": MODEL_NAME,
        "revision": MODEL_REVISION,
        "provider": "CUDA",
        "device_ordinal": device_ordinal,
        "tf32": false,
        "error_on_failure": true,
        "arena_max_bytes": cuda_provider.arena_max_bytes,
        "publisher_parity_cosine": {
            "cpu": comparison.cpu_publisher_parity,
            "cuda": comparison.cuda_publisher_parity
        },
        "cached_bypass_cosine": {
            "query": comparison.query_cosine,
            "source": comparison.source_cosine,
            "minimum": comparison.minimum_cosine
        },
        "batch_consistency_cosine": {
            "cpu_query": comparison.cpu_batch_query_cosine,
            "cpu_source": comparison.cpu_batch_source_cosine,
            "cuda_query": comparison.cuda_batch_query_cosine,
            "cuda_source": comparison.cuda_batch_source_cosine
        },
        "cached_bypass_min_cosine": comparison.minimum_cosine,
        "required_cosine": CPU_CUDA_PARITY_COSINE,
        "total_ms": started.elapsed().as_millis(),
        "scope": "publisher parity gate plus cached-bypass CPU/CUDA and batch consistency comparisons; GPU kernel placement requires an external profiler"
    }))
}

/// Executes real CPU inference against the publisher's frozen example.
pub fn run_gate(model_dir: &Path) -> Result<Value> {
    let started = Instant::now();
    let mut model = super::provider::load_verified_model(model_dir, SemanticProviderConfig::cpu())?;
    let initialized_ms = started.elapsed().as_millis();
    let cosine = check_parity(&mut model)?;
    let total_ms = started.elapsed().as_millis();
    let peak_rss_kib = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse::<u64>().ok())
        });
    if peak_rss_kib.is_some_and(|rss| rss > 4 * 1024 * 1024) {
        bail!("semantic_unavailable: small-input gate exceeded 4 GiB peak RSS");
    }
    Ok(serde_json::json!({
        "status": "passed", "model": MODEL_NAME, "revision": MODEL_REVISION,
        "dimensions": 768, "precision": "f32", "pooling": "masked_mean", "normalization": "l2",
        "provider": "CPU", "intra_threads": 2, "max_input_tokens": MAX_INPUT_TOKENS,
        "max_batch_inputs": MAX_BATCH_INPUTS, "max_batch_padded_tokens": MAX_BATCH_PADDED_TOKENS,
        "publisher_cosine": 0.7281748759529421_f64, "measured_cosine": cosine,
        "absolute_tolerance": 0.002, "initialize_ms": initialized_ms,
        "total_ms": total_ms, "peak_rss_kib": peak_rss_kib,
        "scope": "two published inputs; does not establish retrieval quality or long-input resource bounds"
    }))
}
