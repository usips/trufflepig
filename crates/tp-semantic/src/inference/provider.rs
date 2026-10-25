use super::pinned_assets::{read_verified_asset, verify_asset_digest};
use super::{MAX_INPUT_TOKENS, RERANK_MAX_TOKENS, SemanticProvider, SemanticProviderConfig};
use crate::semantic::VerifiedAssetDigests;
use anyhow::{Context, Result};
use fastembed::{
    InitOptionsUserDefined, OnnxSource, Pooling, RerankInitOptionsUserDefined, TextEmbedding,
    TextRerank, TokenizerFiles, UserDefinedEmbeddingModel, UserDefinedRerankingModel,
};
use std::path::Path;
use tp_core::coded_error::CodedContext;

const MODEL_MANIFEST: &str = include_str!("../../../evaluation/semantic_gate/model.json");
const RERANKER_MANIFEST: &str = include_str!("../../../evaluation/semantic_gate/reranker.json");

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
                    anyhow::anyhow!(coded!(
                        SemanticUnavailable,
                        "CUDA arena limit does not fit this platform"
                    ))
                })?;
                let dispatch = ort::ep::CUDA::default()
                    .with_device_id(device_ordinal)
                    .with_memory_limit(arena_max_bytes)
                    .with_tf32(provider.tf32)
                    .build()
                    .error_on_failure();
                Ok(vec![dispatch])
            }
            #[cfg(not(feature = "semantic-cuda"))]
            {
                let _ = (device_ordinal, provider);
                anyhow::bail!(coded!(
                    SemanticUnavailable,
                    "CUDA provider requires building with --features semantic-cuda"
                ))
            }
        }
    }
}

pub(super) fn load_verified_model(
    model_dir: &Path,
    provider: SemanticProviderConfig,
    digests: &mut VerifiedAssetDigests,
) -> Result<TextEmbedding> {
    let runtime = std::env::var_os("ORT_DYLIB_PATH").with_coded_context(|| {
        coded!(
            SemanticUnavailable,
            "set ORT_DYLIB_PATH to a compatible ONNX Runtime shared library"
        )
    })?;
    load_verified_model_with_runtime(model_dir, provider, Path::new(&runtime), digests)
}

pub(super) fn load_verified_reranker(
    model_dir: &Path,
    provider: SemanticProviderConfig,
    digests: &mut VerifiedAssetDigests,
) -> Result<TextRerank> {
    let runtime = std::env::var_os("ORT_DYLIB_PATH").with_coded_context(|| {
        coded!(
            RerankUnavailable,
            "set ORT_DYLIB_PATH to a compatible ONNX Runtime shared library"
        )
    })?;
    load_verified_reranker_with_runtime(model_dir, provider, Path::new(&runtime), digests)
}

pub(super) fn load_verified_reranker_with_runtime(
    model_dir: &Path,
    provider: SemanticProviderConfig,
    runtime_library: &Path,
    digests: &mut VerifiedAssetDigests,
) -> Result<TextRerank> {
    provider.validate()?;
    ort::init_from(runtime_library)
        .with_coded_context(|| {
            coded!(RerankUnavailable, "cannot load ONNX Runtime shared library")
        })?
        .commit();
    let execution_providers = provider_dispatches(provider)?;
    let manifest: serde_json::Value = serde_json::from_str(RERANKER_MANIFEST)?;
    let assets = manifest["assets"]
        .as_object()
        .context("invalid reranker manifest")?;
    verify_asset_digest(
        model_dir,
        assets,
        "model.onnx",
        "rerank_unavailable:",
        digests,
    )?;
    verify_asset_digest(
        model_dir,
        assets,
        "model.onnx.data",
        "rerank_unavailable:",
        digests,
    )?;
    let tokenizer = TokenizerFiles {
        tokenizer_file: read_verified_asset(
            model_dir,
            assets,
            "tokenizer.json",
            "rerank_unavailable:",
            digests,
        )?,
        config_file: read_verified_asset(
            model_dir,
            assets,
            "config.json",
            "rerank_unavailable:",
            digests,
        )?,
        special_tokens_map_file: read_verified_asset(
            model_dir,
            assets,
            "special_tokens_map.json",
            "rerank_unavailable:",
            digests,
        )?,
        tokenizer_config_file: read_verified_asset(
            model_dir,
            assets,
            "tokenizer_config.json",
            "rerank_unavailable:",
            digests,
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
    let model = TextRerank::try_new_from_user_defined(model, options).with_coded_context(|| {
        coded!(
            RerankUnavailable,
            "ONNX reranker initialization failed; check ORT_DYLIB_PATH and provider configuration"
        )
    })?;
    super::super::MODEL_INITIALIZATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(model)
}

pub(super) fn load_verified_model_with_runtime(
    model_dir: &Path,
    provider: SemanticProviderConfig,
    runtime_library: &Path,
    digests: &mut VerifiedAssetDigests,
) -> Result<TextEmbedding> {
    provider.validate()?;
    ort::init_from(runtime_library)
        .with_coded_context(|| {
            coded!(
                SemanticUnavailable,
                "cannot load ONNX Runtime shared library"
            )
        })?
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
            digests,
        )?,
        config_file: read_verified_asset(
            model_dir,
            assets,
            "config.json",
            "semantic_unavailable:",
            digests,
        )?,
        special_tokens_map_file: read_verified_asset(
            model_dir,
            assets,
            "special_tokens_map.json",
            "semantic_unavailable:",
            digests,
        )?,
        tokenizer_config_file: read_verified_asset(
            model_dir,
            assets,
            "tokenizer_config.json",
            "semantic_unavailable:",
            digests,
        )?,
    };
    let model = UserDefinedEmbeddingModel::new(
        read_verified_asset(
            model_dir,
            assets,
            "onnx/model.onnx",
            "semantic_unavailable:",
            digests,
        )?,
        tokenizer,
    )
    .with_pooling(Pooling::Mean);
    let options = InitOptionsUserDefined::new()
        .with_max_length(MAX_INPUT_TOKENS)
        .with_execution_providers(execution_providers)
        .with_intra_threads(2);
    let mut model =
        TextEmbedding::try_new_from_user_defined(model, options).with_coded_context(|| {
            coded!(
                SemanticUnavailable,
                "ONNX model initialization failed; check ORT_DYLIB_PATH and provider configuration"
            )
        })?;
    super::super::MODEL_INITIALIZATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Reject excessive input explicitly instead of silently truncating content.
    model
        .tokenizer
        .with_truncation(None)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(model)
}
