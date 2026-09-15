use super::{MAX_INPUT_TOKENS, SemanticProvider, SemanticProviderConfig};
use anyhow::{Context, Result, bail};
use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

const MODEL_MANIFEST: &str = include_str!("../../../evaluation/semantic_gate/model.json");

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
    let read_asset = |name: &str| -> Result<Vec<u8>> {
        let bytes = fs::read(model_dir.join(name)).with_context(|| {
            format!(
                "semantic_unavailable: missing pinned asset {name}; see evaluation/semantic_gate/model.json"
            )
        })?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if assets[name]["sha256"].as_str() != Some(digest.as_str()) {
            bail!("semantic_unavailable: pinned asset checksum mismatch: {name}");
        }
        Ok(bytes)
    };
    let tokenizer = TokenizerFiles {
        tokenizer_file: read_asset("tokenizer.json")?,
        config_file: read_asset("config.json")?,
        special_tokens_map_file: read_asset("special_tokens_map.json")?,
        tokenizer_config_file: read_asset("tokenizer_config.json")?,
    };
    let model = UserDefinedEmbeddingModel::new(read_asset("onnx/model.onnx")?, tokenizer)
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
