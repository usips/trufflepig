//! Reads current files into the unpublished store in the given order, under the
//! file, byte, fact, and read-window limits. Each limit stops reading and
//! records the remainder as unsearched instead of failing the scan.
use super::{
    coverage::{CurrentCoverage, LIMIT_BYTES, LIMIT_DEADLINE, LIMIT_FACTS, LIMIT_FILES},
    literal_prefilter::LiteralPrefilter,
    read_cutoff::ReadCutoff,
};
use crate::{
    source,
    store::{self, Store},
};
use anyhow::Result;
use tp_core::coded_error::ErrorCode;

/// Most files extracted by one scan.
pub(super) const MAX_CURRENT_FILES: usize = 4_096;
pub(super) const MAX_CURRENT_FILE_BYTES: usize = 2 * 1024 * 1024;
/// Most bytes extracted by one scan.
const MAX_CURRENT_SOURCE_BYTES: usize = 32 * 1024 * 1024;
/// Most bytes the literal prefilter scans by one scan.
const MAX_PREFILTER_BYTES: usize = 512 * 1024 * 1024;
const MAX_CURRENT_SOURCE_FACTS: usize = 250_000;

pub(super) fn read_current_sources(
    store: &Store,
    paths: &[String],
    mut prefilter: Option<&mut LiteralPrefilter>,
    cutoff: &mut ReadCutoff,
    current: &mut CurrentCoverage,
) -> Result<()> {
    let mut scanned_bytes = 0usize;
    for (index, encoded_path) in paths.iter().enumerate() {
        let remaining = paths.len() - index;
        if cutoff.reached() {
            current.stop(LIMIT_DEADLINE, remaining, Some(encoded_path));
            break;
        }
        if current.read_files == MAX_CURRENT_FILES {
            current.stop(LIMIT_FILES, remaining, Some(encoded_path));
            break;
        }
        let relative = tp_core::path_codec::decode_path(encoded_path)?;
        cutoff.record_read();
        // The prefilter can rule out files too large to extract.
        let read_limit = if prefilter.is_some() {
            source::MAX_READ_BYTES
        } else {
            MAX_CURRENT_FILE_BYTES
        };
        let bytes = match source::read_contained(&store.root, &relative, read_limit) {
            Ok(bytes) => bytes,
            Err(error) if ErrorCode::SourceExcluded.is_code_of(&error) => {
                store::current_source::mark_unavailable(store, encoded_path, "source_excluded", 0)?;
                current.excluded_files += 1;
                current.complete = false;
                continue;
            }
            Err(_) => {
                store::current_source::mark_unavailable(
                    store,
                    encoded_path,
                    "source_unavailable",
                    0,
                )?;
                current.read_failures += 1;
                current.complete = false;
                continue;
            }
        };
        if let Some(prefilter) = prefilter.as_deref_mut() {
            scanned_bytes = scanned_bytes.saturating_add(bytes.len());
            if scanned_bytes > MAX_PREFILTER_BYTES {
                current.stop(LIMIT_BYTES, remaining, Some(encoded_path));
                break;
            }
            if !prefilter.admits(encoded_path, &bytes) {
                current.prefiltered_files += 1;
                continue;
            }
        }
        if bytes.len() > MAX_CURRENT_FILE_BYTES {
            store::current_source::mark_unavailable(
                store,
                encoded_path,
                "source_excluded",
                bytes.len(),
            )?;
            current.excluded_files += 1;
            current.complete = false;
            continue;
        }
        if bytes.contains(&0) {
            store::current_source::mark_unavailable(store, encoded_path, "binary", bytes.len())?;
            current.excluded_files += 1;
            current.complete = false;
            continue;
        }
        let next_bytes = current.bytes_read.saturating_add(bytes.len());
        if next_bytes > MAX_CURRENT_SOURCE_BYTES {
            current.stop(LIMIT_BYTES, remaining, Some(encoded_path));
            break;
        }
        if !cutoff.admits_extraction(bytes.len()) {
            current.stop(LIMIT_DEADLINE, 1, Some(encoded_path));
            continue;
        }
        let extraction = tp_extract::extract(encoded_path, &bytes);
        let facts = extraction_fact_count(&extraction);
        let next_facts = current.facts_indexed.saturating_add(facts);
        if next_facts > MAX_CURRENT_SOURCE_FACTS {
            current.stop(LIMIT_FACTS, remaining, Some(encoded_path));
            break;
        }
        if !matches!(
            extraction.status.as_str(),
            "ok" | "parsed" | "complete" | "lexical_only" | "recovered"
        ) {
            current.parse_failures += 1;
            current.complete = false;
        }
        store::current_source::insert(store, encoded_path, &bytes, &extraction)?;
        current.read_files += 1;
        current.bytes_read = next_bytes;
        current.facts_indexed = next_facts;
    }
    Ok(())
}

pub(super) fn extraction_fact_count(extraction: &tp_extract::Extraction) -> usize {
    extraction
        .definitions
        .len()
        .saturating_add(extraction.occurrences.len())
        .saturating_add(extraction.relationships.len())
}
