//! Durable extraction facts keyed by revision, grammar, and
//! [`extraction_cache_version`] ([cache identity](../../../docs/index-storage-contract.md#extraction-cache-identity)).
use crate::extract::{self, Extraction};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

const MAX_FACTS: usize = 50_000;
pub(in crate::store) const EXTRACTION_CONTRACT_REVISION: u32 = 6;

/// Locked `tree-sitter*` package versions, extracted from `Cargo.lock` by `build.rs`.
const GRAMMAR_VERSIONS: &str = env!("TRUFFLEPIG_GRAMMAR_VERSIONS");

/// Keys durable extractions on the extraction contract and grammar crate
/// versions only, so unrelated dependency changes keep cached facts.
pub(crate) fn extraction_cache_version() -> i64 {
    let mut version = blake3::Hasher::new();
    version.update(b"trufflepig-extraction-cache-v2");
    version.update(&EXTRACTION_CONTRACT_REVISION.to_le_bytes());
    version.update(GRAMMAR_VERSIONS.as_bytes());
    i64::from_le_bytes(
        version.finalize().as_bytes()[..8]
            .try_into()
            .expect("eight hash bytes"),
    )
}

/// The cache grammar key: the path's final extension.
pub(in crate::store) fn grammar(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or("")
}

/// The previous publication's completed extraction for `revision`, if it holds one.
pub(in crate::store) fn published_extraction(
    previous: &Connection,
    path: &str,
    revision: &str,
    cache_version: i64,
) -> Result<Option<Extraction>> {
    let cached: Option<String> = previous
        .prepare_cached(
            "SELECT facts FROM extraction_cache WHERE revision=?1 AND grammar=?2 AND version=?3",
        )?
        .query_row(params![revision, grammar(path), cache_version], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(facts) = cached else {
        return Ok(None);
    };
    let mut extraction: Extraction = serde_json::from_str(&facts)?;
    if extraction.status == "cancelled" {
        return Ok(None);
    }
    limit_facts(&mut extraction);
    Ok(Some(extraction))
}

/// The extraction for `source`, and whether the previous publication's extraction cache served it.
pub(in crate::store) fn cached_extraction(
    previous: &Connection,
    path: &str,
    revision: &str,
    source: &[u8],
    cache_version: i64,
) -> Result<(Extraction, bool)> {
    if let Some(extraction) = published_extraction(previous, path, revision, cache_version)? {
        return Ok((extraction, true));
    }
    let mut extraction = extract::extract(path, source);
    limit_facts(&mut extraction);
    Ok((extraction, false))
}

fn limit_facts(extraction: &mut Extraction) {
    if extraction.definitions.len() + extraction.occurrences.len() + extraction.relationships.len()
        > MAX_FACTS
    {
        extraction.definitions.clear();
        extraction.occurrences.clear();
        extraction.relationships.clear();
        extraction.status = "fact_limit".into();
    }
}

pub(in crate::store) fn insert_extraction_cache(
    staged: &Transaction<'_>,
    revision: &str,
    path: &str,
    cache_version: i64,
    extraction: &Extraction,
) -> Result<()> {
    staged
        .prepare_cached(
            "INSERT OR IGNORE INTO extraction_cache(revision,grammar,version,facts) VALUES(?1,?2,?3,?4)",
        )?
        .execute(params![
            revision,
            grammar(path),
            cache_version,
            serde_json::to_string(extraction)?
        ])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_cache_version_keys_on_grammar_crates_only() {
        assert!(
            GRAMMAR_VERSIONS.contains("tree-sitter="),
            "{GRAMMAR_VERSIONS}"
        );
        assert!(
            GRAMMAR_VERSIONS.contains("tree-sitter-rust="),
            "{GRAMMAR_VERSIONS}"
        );
        assert!(
            GRAMMAR_VERSIONS
                .split(';')
                .all(|entry| entry.starts_with("tree-sitter")),
            "{GRAMMAR_VERSIONS}"
        );
    }
}
