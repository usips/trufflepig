use super::{Coverage, encode_path, regions};
use crate::extract::{self, Extraction};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FACTS: usize = 50_000;

pub(super) fn stage(
    conn: &mut Connection,
    previous: &Connection,
    root: &Path,
    cache: &Path,
) -> Result<(Coverage, String)> {
    let mut coverage = Coverage::default();
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"trufflepig-schema-1-extraction-1");
    let cache_version = i64::from_le_bytes(
        blake3::hash(include_bytes!("../../Cargo.lock")).as_bytes()[..8]
            .try_into()
            .expect("eight hash bytes"),
    );
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
        !entry.file_type().is_some_and(|kind| kind.is_dir())
            || !matches!(
                entry.file_name().to_str(),
                Some(".git" | "target" | "node_modules" | ".trufflepig")
            )
    });
    let transaction = conn.transaction()?;
    for entry in walker.build() {
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
        let extraction = match (source.as_ref(), revision.as_ref()) {
            (Some(source), Some(revision)) => cached_extraction(
                previous,
                &transaction,
                &path,
                revision,
                source,
                cache_version,
            )?,
            _ => Extraction {
                language: extract::language(&path).to_owned(),
                status: status.to_owned(),
                ..Extraction::default()
            },
        };
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
        transaction.execute(
            "INSERT INTO files(path,revision,language,status,bytes) VALUES(?1,?2,?3,?4,?5)",
            params![
                path,
                revision,
                extraction.language,
                extraction.status,
                length as i64
            ],
        )?;
        let file_id = transaction.last_insert_rowid();
        if let (Some(source), Some(revision)) = (source.as_ref(), revision.as_ref()) {
            transaction.execute(
                "INSERT OR IGNORE INTO contents VALUES(?1,?2)",
                params![revision, source],
            )?;
            insert_facts(&transaction, file_id, &extraction)?;
            if extraction.language != "text" {
                transaction.execute("INSERT INTO definitions(file_id,name,kind,start,end,container) VALUES(?1,?2,'module',0,?3,NULL)", params![file_id, path, source.len() as i64])?;
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
    transaction.commit()?;
    Ok((coverage, fingerprint.finalize().to_hex().to_string()))
}

fn cached_extraction(
    previous: &Connection,
    staged: &Transaction<'_>,
    path: &str,
    revision: &str,
    source: &[u8],
    cache_version: i64,
) -> Result<Extraction> {
    let grammar = path.rsplit('.').next().unwrap_or("");
    let cached: Option<String> = previous
        .query_row(
            "SELECT facts FROM extraction_cache WHERE revision=?1 AND grammar=?2 AND version=?3",
            params![revision, grammar, cache_version],
            |row| row.get(0),
        )
        .optional()?;
    let mut extraction: Extraction = match cached.as_deref() {
        Some(facts) => serde_json::from_str(facts)?,
        None => extract::extract(path, source),
    };
    if extraction.status == "cancelled" && cached.is_some() {
        extraction = extract::extract(path, source);
    }
    if extraction.definitions.len() + extraction.occurrences.len() + extraction.relationships.len()
        > MAX_FACTS
    {
        extraction.definitions.clear();
        extraction.occurrences.clear();
        extraction.relationships.clear();
        extraction.status = "fact_limit".into();
    }
    if extraction.status != "cancelled" {
        staged.execute(
            "INSERT OR IGNORE INTO extraction_cache VALUES(?1,?2,?3,?4)",
            params![
                revision,
                grammar,
                cache_version,
                serde_json::to_string(&extraction)?
            ],
        )?;
    }
    Ok(extraction)
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
                if error.to_string().starts_with("source_excluded") {
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

fn insert_facts(tx: &Transaction<'_>, file_id: i64, extraction: &Extraction) -> Result<()> {
    let mut ids = Vec::with_capacity(extraction.definitions.len());
    for definition in &extraction.definitions {
        tx.execute("INSERT INTO definitions(file_id,name,kind,start,end,container) VALUES(?1,?2,?3,?4,?5,?6)", params![file_id, definition.name, definition.kind, definition.start as i64, definition.end as i64, definition.container])?;
        ids.push(tx.last_insert_rowid());
    }
    for occurrence in &extraction.occurrences {
        let target = occurrence.target.and_then(|index| ids.get(index)).copied();
        let candidates: Vec<i64> = occurrence
            .candidates
            .iter()
            .filter_map(|index| ids.get(*index).copied())
            .take(64)
            .collect();
        tx.execute("INSERT INTO occurrences(file_id,name,start,end,role,target,candidates,provenance) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![file_id, occurrence.name, occurrence.start as i64, occurrence.end as i64, occurrence.role, target, serde_json::to_string(&candidates)?, occurrence.provenance])?;
    }
    for relationship in &extraction.relationships {
        let Some(source) = ids.get(relationship.source) else {
            continue;
        };
        let target = relationship
            .target
            .and_then(|index| ids.get(index))
            .copied();
        tx.execute(
            "INSERT INTO relationships VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                source,
                target,
                relationship.kind,
                file_id,
                relationship.evidence_start as i64,
                relationship.evidence_end as i64,
                relationship.provenance
            ],
        )?;
    }
    Ok(())
}
