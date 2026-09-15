use super::super::{DIMENSIONS, Embedding, INPUT_VERSION, MODEL_REVISION};
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};

/// The exact pooling and normalization contract used by the pinned model.
pub(super) const NORMALIZATION: &str = "l2";
const NORM_TOLERANCE: f64 = 0.001;
const MODEL_MANIFEST: &[u8] = include_bytes!("../../../evaluation/semantic_gate/model.json");

#[derive(Debug, Default)]
pub(super) struct CacheCounters {
    pub(super) corrupt_misses: AtomicU64,
    pub(super) provenance_misses: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CacheStatus {
    pub corrupt_misses: u64,
    pub provenance_misses: u64,
}

impl CacheCounters {
    pub(super) fn status(&self) -> CacheStatus {
        CacheStatus {
            corrupt_misses: self.corrupt_misses.load(Ordering::Relaxed),
            provenance_misses: self.provenance_misses.load(Ordering::Relaxed),
        }
    }

    pub(super) fn corrupt_miss(&self) {
        self.corrupt_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn provenance_miss(&self) {
        self.provenance_misses.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) fn manifest_digest() -> String {
    format!("{:x}", Sha256::digest(MODEL_MANIFEST))
}

/// Keys bind source content to every input that can change its vector.
pub fn content_key(text: &str) -> String {
    let dimensions = DIMENSIONS.to_le_bytes();
    let parts: [(&[u8], &[u8]); 6] = [
        (b"manifest", MODEL_MANIFEST),
        (b"model_revision", MODEL_REVISION.as_bytes()),
        (b"input_version", INPUT_VERSION.as_bytes()),
        (b"dimensions", &dimensions),
        (b"normalization", NORMALIZATION.as_bytes()),
        (b"content", text.as_bytes()),
    ];
    let mut digest = Sha256::new();
    for (label, value) in parts {
        digest.update((label.len() as u64).to_le_bytes());
        digest.update(label);
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value);
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn encode(vector: &Embedding) -> Result<[u8; DIMENSIONS * 4]> {
    // Model callers may provide an unnormalized finite output. Normalize only
    // at the write boundary; reads validate persisted bytes before returning.
    let normalized = Embedding::from_values(&vector.0)?;
    let mut bytes = [0_u8; DIMENSIONS * 4];
    for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(normalized.0) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

pub(super) fn decode(
    bytes: &[u8],
    model_revision: Option<&str>,
    input_version: Option<&str>,
    dimensions: Option<i64>,
    normalization: Option<&str>,
    manifest: Option<&str>,
    expected_manifest: &str,
    counters: &CacheCounters,
) -> Option<Embedding> {
    if bytes.len() != DIMENSIONS * 4 {
        counters.corrupt_miss();
        return None;
    }
    let mut values = [0.0; DIMENSIONS];
    for (value, chunk) in values.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *value = f32::from_le_bytes(*chunk);
    }
    if validate_values(&values).is_err() {
        counters.corrupt_miss();
        return None;
    }
    if model_revision != Some(MODEL_REVISION)
        || input_version != Some(INPUT_VERSION)
        || dimensions != Some(DIMENSIONS as i64)
        || normalization != Some(NORMALIZATION)
        || manifest != Some(expected_manifest)
    {
        counters.provenance_miss();
        return None;
    }
    Some(Embedding(values))
}

fn validate_values(values: &[f32; DIMENSIONS]) -> Result<()> {
    if values.iter().any(|value| !value.is_finite()) {
        bail!("invalid_embedding: cached vector contains non-finite values");
    }
    let norm = values
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm <= f64::EPSILON || (norm - 1.0).abs() > NORM_TOLERANCE {
        bail!("invalid_embedding: cached vector is not unit normalized");
    }
    Ok(())
}
