//! A pass's coverage counts and scan fingerprint, accumulated in walk order.
use super::extraction_reuse::{EXTRACTION_CONTRACT_REVISION, extraction_cache_version};
use crate::store::Coverage;

/// Accumulates a pass's coverage and scan fingerprint, file by file in walk order.
pub(in crate::store) struct ScanTally {
    coverage: Coverage,
    fingerprint: blake3::Hasher,
}

impl ScanTally {
    pub(in crate::store) fn new() -> Self {
        let mut fingerprint = blake3::Hasher::new();
        fingerprint.update(b"trufflepig-schema-1-extraction");
        fingerprint.update(&EXTRACTION_CONTRACT_REVISION.to_le_bytes());
        fingerprint.update(&extraction_cache_version().to_le_bytes());
        Self {
            coverage: Coverage::default(),
            fingerprint,
        }
    }

    pub(in crate::store) fn file(
        &mut self,
        path: &str,
        read_status: &str,
        length: u64,
        revision: Option<&str>,
        extraction_status: &str,
        has_source: bool,
    ) {
        let coverage = &mut self.coverage;
        coverage.total_files += 1;
        let fingerprint = &mut self.fingerprint;
        fingerprint.update(&(path.len() as u64).to_le_bytes());
        fingerprint.update(path.as_bytes());
        fingerprint.update(read_status.as_bytes());
        fingerprint.update(&length.to_le_bytes());
        if let Some(revision) = revision {
            fingerprint.update(revision.as_bytes());
        }
        fingerprint.update(extraction_status.as_bytes());
        if extraction_status == "fact_limit" {
            coverage.truncated_files += 1;
        }
        if has_source {
            coverage.indexed_files += 1;
            coverage.indexed_bytes += length;
            if !matches!(
                extraction_status,
                "ok" | "parsed" | "complete" | "lexical_only" | "recovered"
            ) {
                coverage.parse_failures += 1;
            }
        } else {
            coverage.excluded_files += 1;
        }
    }

    pub(in crate::store) fn finish(mut self, walk_failures: usize) -> (Coverage, String) {
        self.coverage.walk_failures = walk_failures;
        self.fingerprint.update(&walk_failures.to_le_bytes());
        (
            self.coverage,
            self.fingerprint.finalize().to_hex().to_string(),
        )
    }
}
