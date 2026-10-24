use super::{Coverage, encode_path, index_checkpoint::IndexCheckpoint, index_deadline, regions};
use crate::daemon::deadline::QueryDeadline;
use crate::extract::{self, Extraction};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

pub(crate) const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FACTS: usize = 50_000;
const EXTRACTION_CONTRACT_REVISION: u32 = 6;

pub(super) fn stage(
    conn: &mut Connection,
    previous: &Connection,
    root: &Path,
    cache: &Path,
) -> Result<(Coverage, String)> {
    stage_inner(conn, previous, root, cache, None, None)
}

pub(super) fn stage_checkpointed(
    conn: &mut Connection,
    previous: &Connection,
    root: &Path,
    cache: &Path,
    checkpoint: &mut IndexCheckpoint,
    deadline: Option<&QueryDeadline>,
) -> Result<(Coverage, String)> {
    stage_inner(conn, previous, root, cache, deadline, Some(checkpoint))
}

fn stage_inner(
    conn: &mut Connection,
    previous: &Connection,
    root: &Path,
    cache: &Path,
    deadline: Option<&QueryDeadline>,
    mut checkpoint: Option<&mut IndexCheckpoint>,
) -> Result<(Coverage, String)> {
    if let Some(deadline) = deadline {
        index_deadline::check(deadline)?;
    }
    let mut coverage = Coverage::default();
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"trufflepig-schema-1-extraction");
    fingerprint.update(&EXTRACTION_CONTRACT_REVISION.to_le_bytes());
    let cache_version = extraction_cache_version();
    fingerprint.update(&cache_version.to_le_bytes());
    let excluded_cache = cache.to_path_buf();
    let mut walker = ignore::WalkBuilder::new(root);
    walker
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b));
    walker.filter_entry(move |entry| {
        if entry.path().starts_with(&excluded_cache) {
            return false;
        }
        // A linked worktree's `.git` is a file; skip it whatever its type.
        entry.file_name() != ".git"
            && (!entry.file_type().is_some_and(|kind| kind.is_dir())
                || !matches!(
                    entry.file_name().to_str(),
                    Some("target" | "node_modules" | ".trufflepig")
                ))
    });
    let transaction = conn.transaction()?;
    for entry in walker.build() {
        if let Some(deadline) = deadline {
            index_deadline::check(deadline)?;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                coverage.walk_failures += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        coverage.total_files += 1;
        let relative = entry.path().strip_prefix(root)?;
        let path = encode_path(relative);
        let (source, status, length) = read_source(root, relative);
        if let Some(deadline) = deadline {
            index_deadline::check(deadline)?;
        }
        let revision = source
            .as_ref()
            .map(|source| blake3::hash(source).to_hex().to_string());
        fingerprint.update(&(path.len() as u64).to_le_bytes());
        fingerprint.update(path.as_bytes());
        fingerprint.update(status.as_bytes());
        fingerprint.update(&length.to_le_bytes());
        if let Some(revision) = &revision {
            fingerprint.update(revision.as_bytes());
        }
        let extraction = if let Some(checkpoint) = checkpoint.as_deref_mut() {
            match checkpoint.extraction(
                &path,
                revision.as_deref(),
                status,
                length,
                cache_version,
            )? {
                Some(extraction) => {
                    checkpoint.reused_file(&path)?;
                    extraction
                }
                None => {
                    let (extraction, from_publication) = match (source.as_ref(), revision.as_ref())
                    {
                        (Some(source), Some(revision)) => {
                            cached_extraction(previous, &path, revision, source, cache_version)?
                        }
                        _ => (
                            Extraction {
                                language: extract::language(&path).to_owned(),
                                status: status.to_owned(),
                                ..Extraction::default()
                            },
                            false,
                        ),
                    };
                    if let Some(deadline) = deadline {
                        index_deadline::check(deadline)?;
                    }
                    if from_publication {
                        checkpoint.publication_reused_file()?;
                    } else if revision.is_some()
                        && extraction.status != "cancelled"
                        && checkpoint.retains_new_extractions()
                    {
                        checkpoint.completed_file(
                            &path,
                            revision.as_deref(),
                            status,
                            length,
                            cache_version,
                            &extraction,
                        )?;
                    }
                    extraction
                }
            }
        } else {
            match (source.as_ref(), revision.as_ref()) {
                (Some(source), Some(revision)) => {
                    cached_extraction(previous, &path, revision, source, cache_version)?.0
                }
                _ => Extraction {
                    language: extract::language(&path).to_owned(),
                    status: status.to_owned(),
                    ..Extraction::default()
                },
            }
        };
        if let Some(deadline) = deadline {
            index_deadline::check(deadline)?;
        }
        if extraction.status == "fact_limit" {
            coverage.truncated_files += 1;
        }
        fingerprint.update(extraction.status.as_bytes());
        if source.is_some() {
            coverage.indexed_files += 1;
            coverage.indexed_bytes += length;
            if !matches!(
                extraction.status.as_str(),
                "ok" | "parsed" | "complete" | "lexical_only" | "recovered"
            ) {
                coverage.parse_failures += 1;
            }
        } else {
            coverage.excluded_files += 1;
        }
        transaction
            .prepare_cached(
                "INSERT INTO files(path,revision,language,status,bytes) VALUES(?1,?2,?3,?4,?5)",
            )?
            .execute(params![
                path,
                revision,
                extraction.language,
                extraction.status,
                length as i64
            ])?;
        let file_id = transaction.last_insert_rowid();
        if let (Some(source), Some(revision)) = (source.as_ref(), revision.as_ref()) {
            if extraction.status != "cancelled" {
                insert_extraction_cache(&transaction, revision, &path, cache_version, &extraction)?;
            }
            transaction
                .prepare_cached("INSERT OR IGNORE INTO contents(revision,bytes) VALUES(?1,?2)")?
                .execute(params![revision, source])?;
            insert_facts(&transaction, file_id, &extraction)?;
            if extraction.language != "text" {
                insert_module_definition(&transaction, file_id, &path, source.len())?;
            }
            regions::insert(
                &transaction,
                file_id,
                &path,
                source,
                &extraction.definitions,
            )?;
        } else {
            regions::insert(&transaction, file_id, &path, &[], &[])?;
        }
    }
    fingerprint.update(&coverage.walk_failures.to_le_bytes());
    if let Some(deadline) = deadline {
        index_deadline::check(deadline)?;
    }
    transaction.commit()?;
    if let Some(checkpoint) = checkpoint.as_deref_mut() {
        if coverage.walk_failures == 0 {
            checkpoint.pass_completed()?;
        }
    }
    Ok((coverage, fingerprint.finalize().to_hex().to_string()))
}

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

pub(crate) fn resolved_fingerprint(scan_fingerprint: &str, resolver_revision: &str) -> String {
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"trufflepig-resolved-index-v1");
    fingerprint.update(scan_fingerprint.as_bytes());
    fingerprint.update(resolver_revision.as_bytes());
    fingerprint.finalize().to_hex().to_string()
}

/// The extraction for `source`, and whether the previous publication's extraction cache served it.
fn cached_extraction(
    previous: &Connection,
    path: &str,
    revision: &str,
    source: &[u8],
    cache_version: i64,
) -> Result<(Extraction, bool)> {
    let grammar = path.rsplit('.').next().unwrap_or("");
    let cached: Option<String> = previous
        .prepare_cached(
            "SELECT facts FROM extraction_cache WHERE revision=?1 AND grammar=?2 AND version=?3",
        )?
        .query_row(params![revision, grammar, cache_version], |row| row.get(0))
        .optional()?;
    let mut from_publication = cached.is_some();
    let mut extraction: Extraction = match cached.as_deref() {
        Some(facts) => serde_json::from_str(facts)?,
        None => extract::extract(path, source),
    };
    if extraction.status == "cancelled" && cached.is_some() {
        extraction = extract::extract(path, source);
        from_publication = false;
    }
    if extraction.definitions.len() + extraction.occurrences.len() + extraction.relationships.len()
        > MAX_FACTS
    {
        extraction.definitions.clear();
        extraction.occurrences.clear();
        extraction.relationships.clear();
        extraction.status = "fact_limit".into();
    }
    Ok((extraction, from_publication))
}

fn insert_extraction_cache(
    staged: &Transaction<'_>,
    revision: &str,
    path: &str,
    cache_version: i64,
    extraction: &Extraction,
) -> Result<()> {
    let grammar = path.rsplit('.').next().unwrap_or("");
    staged
        .prepare_cached(
            "INSERT OR IGNORE INTO extraction_cache(revision,grammar,version,facts) VALUES(?1,?2,?3,?4)",
        )?
        .execute(params![
            revision,
            grammar,
            cache_version,
            serde_json::to_string(extraction)?
        ])?;
    Ok(())
}

fn read_source(root: &Path, relative: &Path) -> (Option<Vec<u8>>, &'static str, u64) {
    let metadata = match root.join(relative).symlink_metadata() {
        Ok(metadata) => metadata,
        Err(_) => return (None, "metadata_error", 0),
    };
    let length = metadata.len();
    if length > MAX_SOURCE_BYTES {
        return (None, "resource_excluded", length);
    }
    let source = match crate::source::read_contained(root, relative, MAX_SOURCE_BYTES as usize) {
        Ok(source) => source,
        Err(error) => {
            return (
                None,
                if crate::coded_error::ErrorCode::SourceExcluded.is_code_of(&error) {
                    "resource_excluded"
                } else {
                    "read_error"
                },
                length,
            );
        }
    };
    if source.contains(&0) {
        return (None, "binary_excluded", source.len() as u64);
    }
    let length = source.len() as u64;
    (Some(source), "read", length)
}

/// Inserts one file's extracted facts with statements compiled once per connection.
pub(super) fn insert_facts(
    tx: &Transaction<'_>,
    file_id: i64,
    extraction: &Extraction,
) -> Result<()> {
    let mut ids = Vec::with_capacity(extraction.definitions.len());
    let mut definitions = tx.prepare_cached(
        "INSERT INTO definitions(file_id,name,kind,start,end,container) VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    for definition in &extraction.definitions {
        ids.push(definitions.insert(params![
            file_id,
            definition.name,
            definition.kind,
            definition.start as i64,
            definition.end as i64,
            definition.container
        ])?);
    }
    let mut occurrences = tx.prepare_cached(
        "INSERT INTO occurrences(file_id,name,start,end,role,target,candidates,provenance) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
    )?;
    let mut candidates = String::new();
    for occurrence in &extraction.occurrences {
        let target = occurrence.target.and_then(|index| ids.get(index)).copied();
        candidate_json(&ids, &occurrence.candidates, &mut candidates);
        occurrences.execute(params![
            file_id,
            occurrence.name,
            occurrence.start as i64,
            occurrence.end as i64,
            occurrence.role,
            target,
            candidates,
            occurrence.provenance
        ])?;
    }
    let mut relationships = tx.prepare_cached(
        "INSERT INTO relationships(source,target,kind,file_id,start,end,provenance) VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    for relationship in &extraction.relationships {
        let Some(source) = ids.get(relationship.source) else {
            continue;
        };
        let target = relationship
            .target
            .and_then(|index| ids.get(index))
            .copied();
        relationships.execute(params![
            source,
            target,
            relationship.kind,
            file_id,
            relationship.evidence_start as i64,
            relationship.evidence_end as i64,
            relationship.provenance
        ])?;
    }
    Ok(())
}

/// The file-level `module` definition that spans a non-text source.
pub(super) fn insert_module_definition(
    tx: &Transaction<'_>,
    file_id: i64,
    path: &str,
    length: usize,
) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO definitions(file_id,name,kind,start,end,container) VALUES(?1,?2,'module',0,?3,NULL)",
    )?
    .execute(params![file_id, path, length as i64])?;
    Ok(())
}

/// Writes up to 64 resolved candidate ids as a JSON array into `buffer`;
/// readers parse the column as JSON, so an empty set is `[]`.
fn candidate_json(ids: &[i64], indexes: &[usize], buffer: &mut String) {
    use std::fmt::Write;
    buffer.clear();
    buffer.push('[');
    let resolved = indexes.iter().filter_map(|index| ids.get(*index)).take(64);
    for (position, id) in resolved.enumerate() {
        if position > 0 {
            buffer.push(',');
        }
        let _ = write!(buffer, "{id}");
    }
    buffer.push(']');
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

    #[test]
    fn candidate_json_keeps_resolved_ids_up_to_the_bound() {
        let mut buffer = String::new();
        candidate_json(&[10, 20], &[], &mut buffer);
        assert_eq!(buffer, "[]");
        candidate_json(&[10, 20], &[1, 7, 0], &mut buffer);
        assert_eq!(buffer, "[20,10]");
        let ids: Vec<i64> = (0..100).collect();
        let indexes: Vec<usize> = (0..100).collect();
        candidate_json(&ids, &indexes, &mut buffer);
        assert_eq!(serde_json::from_str::<Vec<i64>>(&buffer).unwrap().len(), 64);
    }
}
