use super::{MAX_INPUT_TOKENS, RERANK_MAX_TOKENS, SemanticProvider, SemanticProviderConfig};
use anyhow::{Context, Result, bail};
use fastembed::{
    InitOptionsUserDefined, OnnxSource, Pooling, RerankInitOptionsUserDefined, TextEmbedding,
    TextRerank, TokenizerFiles, UserDefinedEmbeddingModel, UserDefinedRerankingModel,
};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

const MODEL_MANIFEST: &str = include_str!("../../../evaluation/semantic_gate/model.json");
const RERANKER_MANIFEST: &str = include_str!("../../../evaluation/semantic_gate/reranker.json");
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Verifies `name` in `model_dir` against the manifest's pinned sha256 by
/// streaming it through a fixed 64 KiB buffer, without holding the whole
/// file in memory. `error_prefix` (e.g. `"semantic_unavailable:"`) distinguishes
/// embedder from reranker asset failures.
pub(super) fn verify_asset_digest(
    model_dir: &Path,
    assets: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    error_prefix: &str,
) -> Result<()> {
    let mut file = fs::File::open(model_dir.join(name)).with_context(|| {
        format!("{error_prefix} missing pinned asset {name}; see evaluation/semantic_gate/")
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("{error_prefix} cannot read pinned asset {name}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = format!("{:x}", hasher.finalize());
    let asset = assets
        .get(name)
        .with_context(|| format!("{error_prefix} pinned asset missing from manifest: {name}"))?;
    if asset["sha256"].as_str() != Some(digest.as_str()) {
        bail!("{error_prefix} pinned asset checksum mismatch: {name}");
    }
    Ok(())
}

/// Verifies `name` (see [`verify_asset_digest`]) and returns its bytes, for
/// the small tokenizer/config files (and the embedder's in-memory model)
/// that need their content, not just a checksum.
pub(super) fn read_verified_asset(
    model_dir: &Path,
    assets: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    error_prefix: &str,
) -> Result<Vec<u8>> {
    verify_asset_digest(model_dir, assets, name, error_prefix)?;
    fs::read(model_dir.join(name)).with_context(|| {
        format!("{error_prefix} missing pinned asset {name}; see evaluation/semantic_gate/")
    })
}

pub(super) fn provider_dispatches(
    provider: SemanticProviderConfig,
) -> Result<Vec<ort::ep::ExecutionProviderDispatch>> {
    provider.validate()?;
    match provider.provider {
        SemanticProvider::Cpu => Ok(Vec::new()),
        SemanticProvider::Cuda { device_ordinal } => {
            #[cfg(feature = "semantic-cuda")]
            {
                let arena_max_bytes = usize::try_from(provider.arena_max_bytes).map_err(|_| {
                    anyhow::anyhow!(
                        "semantic_unavailable: CUDA arena limit does not fit this platform"
                    )
                })?;
                let dispatch = ort::ep::CUDA::default()
                    .with_device_id(device_ordinal)
                    .with_memory_limit(arena_max_bytes)
                    .with_tf32(false)
                    .build()
                    .error_on_failure();
                Ok(vec![dispatch])
            }
            #[cfg(not(feature = "semantic-cuda"))]
            {
                let _ = (device_ordinal, provider);
                bail!(
                    "semantic_unavailable: CUDA provider requires building with --features semantic-cuda"
                )
            }
        }
    }
}

pub(super) fn load_verified_model(
    model_dir: &Path,
    provider: SemanticProviderConfig,
) -> Result<TextEmbedding> {
    let runtime = std::env::var_os("ORT_DYLIB_PATH").context(
        "semantic_unavailable: set ORT_DYLIB_PATH to a compatible ONNX Runtime shared library",
    )?;
    load_verified_model_with_runtime(model_dir, provider, Path::new(&runtime))
}

pub(super) fn load_verified_reranker(
    model_dir: &Path,
    provider: SemanticProviderConfig,
) -> Result<TextRerank> {
    let runtime = std::env::var_os("ORT_DYLIB_PATH").context(
        "rerank_unavailable: set ORT_DYLIB_PATH to a compatible ONNX Runtime shared library",
    )?;
    load_verified_reranker_with_runtime(model_dir, provider, Path::new(&runtime))
}

pub(super) fn load_verified_reranker_with_runtime(
    model_dir: &Path,
    provider: SemanticProviderConfig,
    runtime_library: &Path,
) -> Result<TextRerank> {
    provider.validate()?;
    ort::init_from(runtime_library)
        .context("rerank_unavailable: cannot load ONNX Runtime shared library")?
        .commit();
    let execution_providers = provider_dispatches(provider)?;
    let manifest: serde_json::Value = serde_json::from_str(RERANKER_MANIFEST)?;
    let assets = manifest["assets"]
        .as_object()
        .context("invalid reranker manifest")?;
    verify_asset_digest(model_dir, assets, "model.onnx", "rerank_unavailable:")?;
    verify_asset_digest(model_dir, assets, "model.onnx.data", "rerank_unavailable:")?;
    let tokenizer = TokenizerFiles {
        tokenizer_file: read_verified_asset(
            model_dir,
            assets,
            "tokenizer.json",
            "rerank_unavailable:",
        )?,
        config_file: read_verified_asset(model_dir, assets, "config.json", "rerank_unavailable:")?,
        special_tokens_map_file: read_verified_asset(
            model_dir,
            assets,
            "special_tokens_map.json",
            "rerank_unavailable:",
        )?,
        tokenizer_config_file: read_verified_asset(
            model_dir,
            assets,
            "tokenizer_config.json",
            "rerank_unavailable:",
        )?,
    };
    // model.onnx references external data model.onnx.data by relative path, so
    // the graph must be loaded from disk rather than from verified-in-memory bytes.
    let model =
        UserDefinedRerankingModel::new(OnnxSource::File(model_dir.join("model.onnx")), tokenizer);
    let options = RerankInitOptionsUserDefined::new()
        .with_max_length(RERANK_MAX_TOKENS)
        .with_execution_providers(execution_providers)
        .with_intra_threads(2);
    let model = TextRerank::try_new_from_user_defined(model, options).context(
        "rerank_unavailable: ONNX reranker initialization failed; check ORT_DYLIB_PATH and provider configuration",
    )?;
    super::super::MODEL_INITIALIZATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(model)
}

pub(super) fn load_verified_model_with_runtime(
    model_dir: &Path,
    provider: SemanticProviderConfig,
    runtime_library: &Path,
) -> Result<TextEmbedding> {
    provider.validate()?;
    ort::init_from(runtime_library)
        .context("semantic_unavailable: cannot load ONNX Runtime shared library")?
        .commit();
    let execution_providers = provider_dispatches(provider)?;
    let manifest: serde_json::Value = serde_json::from_str(MODEL_MANIFEST)?;
    let assets = manifest["assets"]
        .as_object()
        .context("invalid model manifest")?;
    let tokenizer = TokenizerFiles {
        tokenizer_file: read_verified_asset(
            model_dir,
            assets,
            "tokenizer.json",
            "semantic_unavailable:",
        )?,
        config_file: read_verified_asset(
            model_dir,
            assets,
            "config.json",
            "semantic_unavailable:",
        )?,
        special_tokens_map_file: read_verified_asset(
            model_dir,
            assets,
            "special_tokens_map.json",
            "semantic_unavailable:",
        )?,
        tokenizer_config_file: read_verified_asset(
            model_dir,
            assets,
            "tokenizer_config.json",
            "semantic_unavailable:",
        )?,
    };
    let model = UserDefinedEmbeddingModel::new(
        read_verified_asset(
            model_dir,
            assets,
            "onnx/model.onnx",
            "semantic_unavailable:",
        )?,
        tokenizer,
    )
    .with_pooling(Pooling::Mean);
    let options = InitOptionsUserDefined::new()
        .with_max_length(MAX_INPUT_TOKENS)
        .with_execution_providers(execution_providers)
        .with_intra_threads(2);
    let mut model = TextEmbedding::try_new_from_user_defined(model, options).context(
        "semantic_unavailable: ONNX model initialization failed; check ORT_DYLIB_PATH and provider configuration",
    )?;
    super::super::MODEL_INITIALIZATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Reject excessive input explicitly instead of silently truncating content.
    model
        .tokenizer
        .with_truncation(None)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(model)
}
