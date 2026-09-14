use super::{
    Embedding, INPUT_VERSION, MODEL_NAME, MODEL_REVISION, embedding_cache::EmbeddingCache,
};
use anyhow::{Context, Result, bail};
use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, time::Instant};

const MODEL_MANIFEST: &str = include_str!("../../evaluation/semantic_gate/model.json");
const PARITY_QUERY: &str =
    "How do I access the index while iterating over a sequence with a for loop?";
const PARITY_SOURCE: &str =
    "# Use the built-in enumerator\nfor idx, x in enumerate(xs):\n    print(idx, x)";

pub struct SemanticEngine {
    model: TextEmbedding,
    cache: EmbeddingCache,
}

impl SemanticEngine {
    pub fn open(model_dir: &Path, cache_dir: &Path) -> Result<Self> {
        let mut model = load_verified_model(model_dir)?;
        check_parity(&mut model)?;
        Ok(Self {
            model,
            cache: EmbeddingCache::open(cache_dir)?,
        })
    }

    /// Serial mutable access bounds inference concurrency to one per engine.
    pub fn embed(&mut self, text: &str) -> Result<Embedding> {
        let key = content_key(text);
        if let Some(vector) = self.cache.get(&key)? {
            return Ok(vector);
        }
        let vector = infer(&mut self.model, text)?;
        self.cache.put(&key, &vector)?;
        Ok(vector)
    }
}

fn content_key(text: &str) -> String {
    let mut digest = Sha256::new();
    for part in [
        MODEL_REVISION.as_bytes(),
        INPUT_VERSION.as_bytes(),
        text.as_bytes(),
    ] {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    format!("{:x}", digest.finalize())
}

fn load_verified_model(model_dir: &Path) -> Result<TextEmbedding> {
    let runtime = std::env::var_os("ORT_DYLIB_PATH").context(
        "semantic_unavailable: set ORT_DYLIB_PATH to a compatible ONNX Runtime shared library",
    )?;
    ort::init_from(Path::new(&runtime))
        .context("semantic_unavailable: cannot load ONNX Runtime shared library")?
        .commit();
    let manifest: serde_json::Value = serde_json::from_str(MODEL_MANIFEST)?;
    let assets = manifest["assets"]
        .as_object()
        .context("invalid model manifest")?;
    let read_asset = |name: &str| -> Result<Vec<u8>> {
        let bytes = fs::read(model_dir.join(name))
            .with_context(|| format!("semantic_unavailable: missing pinned asset {name}; see evaluation/semantic_gate/model.json"))?;
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
        .with_max_length(8192)
        .with_intra_threads(2);
    let mut model = TextEmbedding::try_new_from_user_defined(model, options).context(
        "semantic_unavailable: CPU ONNX model initialization failed; check ORT_DYLIB_PATH",
    )?;
    // Reject excessive input explicitly instead of silently truncating content.
    model
        .tokenizer
        .with_truncation(None)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(model)
}

fn infer(model: &mut TextEmbedding, text: &str) -> Result<Embedding> {
    let encoded = model
        .tokenizer
        .encode(text, true)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if encoded.len() > 8192 {
        bail!("semantic_unavailable: input exceeds 8192 model tokens");
    }
    let vectors = model
        .embed([text], Some(1))
        .context("semantic_unavailable: inference failed")?;
    Embedding::from_values(vectors.first().context("missing model output")?)
}

fn check_parity(model: &mut TextEmbedding) -> Result<f32> {
    let query = infer(model, PARITY_QUERY)?;
    let source = infer(model, PARITY_SOURCE)?;
    let cosine = query.cosine(&source);
    if (cosine - 0.7281749).abs() > 0.002 {
        bail!("semantic_unavailable: publisher parity gate failed: cosine {cosine}");
    }
    Ok(cosine)
}

/// Executes real CPU inference against the publisher's frozen example.
pub fn run_gate(model_dir: &Path) -> Result<serde_json::Value> {
    let started = Instant::now();
    let mut model = load_verified_model(model_dir)?;
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
        "provider": "CPU", "intra_threads": 2, "max_input_tokens": 8192,
        "publisher_cosine": 0.7281748759529421_f64, "measured_cosine": cosine,
        "absolute_tolerance": 0.002, "initialize_ms": initialized_ms,
        "total_ms": total_ms, "peak_rss_kib": peak_rss_kib,
        "scope": "two published inputs; does not establish retrieval quality or long-input resource bounds"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semantic_content_key_is_content_specific() {
        assert_eq!(content_key("same source"), content_key("same source"));
        assert_ne!(content_key("same source"), content_key("changed source"));
    }
}
