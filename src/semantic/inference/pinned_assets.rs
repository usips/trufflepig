//! Pinned model asset verification against manifest SHA-256 digests.
//! A [`VerifiedAssetDigests`] memo lets a reload skip unchanged files.

use crate::semantic::VerifiedAssetDigests;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Verifies `name` in `model_dir` against the manifest's pinned sha256 by
/// streaming it through a fixed 64 KiB buffer, without holding the whole
/// file in memory. A file `digests` already verified, by unchanged identity,
/// is not hashed again. `error_prefix` (e.g. `"semantic_unavailable:"`)
/// distinguishes embedder from reranker asset failures.
pub(super) fn verify_asset_digest(
    model_dir: &Path,
    assets: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    error_prefix: &str,
    digests: &mut VerifiedAssetDigests,
) -> Result<()> {
    let expected = pinned_digest(assets, name, error_prefix)?;
    let (mut file, metadata) = open_asset(model_dir, name, error_prefix)?;
    if digests.contains(&metadata, expected) {
        return Ok(());
    }
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
    check_digest(hasher.finalize().as_slice(), expected, name, error_prefix)?;
    digests.record(&metadata, expected);
    Ok(())
}

/// Reads `name` once and verifies those bytes (see [`verify_asset_digest`]),
/// for the small tokenizer/config files (and the embedder's in-memory model)
/// that need their content, not just a checksum.
pub(super) fn read_verified_asset(
    model_dir: &Path,
    assets: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    error_prefix: &str,
    digests: &mut VerifiedAssetDigests,
) -> Result<Vec<u8>> {
    let expected = pinned_digest(assets, name, error_prefix)?;
    let (mut file, metadata) = open_asset(model_dir, name, error_prefix)?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)
        .with_context(|| format!("{error_prefix} cannot read pinned asset {name}"))?;
    if !digests.contains(&metadata, expected) {
        check_digest(
            Sha256::digest(&bytes).as_slice(),
            expected,
            name,
            error_prefix,
        )?;
        digests.record(&metadata, expected);
    }
    Ok(bytes)
}

fn pinned_digest<'a>(
    assets: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
    error_prefix: &str,
) -> Result<&'a str> {
    assets
        .get(name)
        .and_then(|asset| asset["sha256"].as_str())
        .with_context(|| format!("{error_prefix} pinned asset missing from manifest: {name}"))
}

fn open_asset(
    model_dir: &Path,
    name: &str,
    error_prefix: &str,
) -> Result<(fs::File, fs::Metadata)> {
    let file = fs::File::open(model_dir.join(name)).with_context(|| {
        format!("{error_prefix} missing pinned asset {name}; see evaluation/semantic_gate/")
    })?;
    let metadata = file
        .metadata()
        .with_context(|| format!("{error_prefix} cannot stat pinned asset {name}"))?;
    Ok((file, metadata))
}

fn check_digest(actual: &[u8], expected: &str, name: &str, error_prefix: &str) -> Result<()> {
    let actual = actual
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected {
        bail!("{error_prefix} pinned asset checksum mismatch: {name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn manifest_for(name: &str, bytes: &[u8]) -> serde_json::Map<String, serde_json::Value> {
        let digest = format!("{:x}", Sha256::digest(bytes));
        let mut assets = serde_json::Map::new();
        assets.insert(name.into(), serde_json::json!({ "sha256": digest }));
        assets
    }

    #[test]
    fn verified_digest_is_reused_until_file_identity_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.onnx.data");
        fs::write(&path, b"pinned weights").unwrap();
        let assets = manifest_for("model.onnx.data", b"pinned weights");
        let mut digests = VerifiedAssetDigests::default();
        verify_asset_digest(
            dir.path(),
            &assets,
            "model.onnx.data",
            "test:",
            &mut digests,
        )
        .unwrap();

        // Same inode, size, and mtime: the memo answers without hashing, so
        // even same-length tampered bytes pass (the identity is the contract).
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"tampered bytes").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        verify_asset_digest(
            dir.path(),
            &assets,
            "model.onnx.data",
            "test:",
            &mut digests,
        )
        .unwrap();

        // A new mtime changes the identity, so the file is hashed again.
        let later = SystemTime::now() + Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let error = verify_asset_digest(
            dir.path(),
            &assets,
            "model.onnx.data",
            "test:",
            &mut digests,
        )
        .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
        let error = read_verified_asset(
            dir.path(),
            &assets,
            "model.onnx.data",
            "test:",
            &mut digests,
        )
        .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn read_verified_asset_returns_the_hashed_bytes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), b"{}").unwrap();
        let assets = manifest_for("config.json", b"{}");
        let mut digests = VerifiedAssetDigests::default();
        let bytes =
            read_verified_asset(dir.path(), &assets, "config.json", "test:", &mut digests).unwrap();
        assert_eq!(bytes, b"{}");
        let missing =
            read_verified_asset(dir.path(), &assets, "absent.json", "test:", &mut digests)
                .unwrap_err();
        assert!(missing.to_string().contains("missing from manifest"));
    }
}
