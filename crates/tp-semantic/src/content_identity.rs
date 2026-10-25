//! Content keys bind source text to every input that can change its vector.
//! Preparation computes them in every build and the embedding cache stores
//! vectors under them, so one definition keeps both in agreement.
use super::{DIMENSIONS, INPUT_VERSION, MODEL_REVISION};
use sha2::{Digest, Sha256};

/// The exact pooling and normalization contract used by the pinned model.
pub(super) const NORMALIZATION: &str = "l2";
/// The pinned model and tokenizer manifest.
pub(super) const MODEL_MANIFEST: &[u8] =
    include_bytes!("../../../evaluation/semantic_gate/model.json");

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
