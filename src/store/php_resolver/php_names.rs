use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

#[cfg(test)]
mod tests;

const MAX_CANDIDATES: usize = 64;
const OCCURRENCE_BATCH: usize = 512;

pub(super) struct PhpClassCandidate {
    pub(super) definition_id: i64,
    pub(super) file_id: i64,
    pub(super) file_path: String,
}

pub(super) struct PhpClassIndex {
    by_fqcn: HashMap<String, Vec<PhpClassCandidate>>,
}

impl PhpClassIndex {
    pub(super) fn candidates(&self, fqcn: &str) -> &[PhpClassCandidate] {
        normalized_fqcn(fqcn)
            .and_then(|key| self.by_fqcn.get(&key))
            .map_or(&[], Vec::as_slice)
    }
}

struct PhpNameOccurrence {
    id: i64,
    file_id: i64,
    name: String,
    start: i64,
    end: i64,
    role: String,
    candidates: String,
    provenance: String,
}

/// Indexes PHP classes, interfaces, traits, and enums by case-insensitive FQCN.
pub(super) fn class_index(conn: &Connection) -> Result<PhpClassIndex> {
    let class_count: i64 = conn.query_row(
        "SELECT count(*) FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.language='php' AND d.kind IN ('class','interface','trait','enum')",
        [],
        |row| row.get(0),
    )?;
    let mut by_fqcn =
        HashMap::<String, Vec<PhpClassCandidate>>::with_capacity(usize::try_from(class_count)?);
    {
        let mut statement = conn.prepare(
            "SELECT d.id,d.file_id,d.name,d.container,f.path FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.language='php' AND d.kind IN ('class','interface','trait','enum') ORDER BY d.id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let definition_id: i64 = row.get(0)?;
            let file_id: i64 = row.get(1)?;
            let name: String = row.get(2)?;
            let namespace: Option<String> = row.get(3)?;
            let file_path: String = row.get(4)?;
            let Some(fqcn) = definition_fqcn(namespace.as_deref(), &name) else {
                continue;
            };
            if is_xfcp_name(&fqcn) {
                continue;
            }
            let Some(key) = normalized_fqcn(&fqcn) else {
                continue;
            };
            by_fqcn.entry(key).or_default().push(PhpClassCandidate {
                definition_id,
                file_id,
                file_path,
            });
        }
    }
    Ok(PhpClassIndex { by_fqcn })
}

/// Adds bounded cross-file candidates for PHP class-name references.
pub(super) fn resolve(conn: &mut Connection, class_index: &PhpClassIndex) -> Result<()> {
    let transaction = conn.transaction()?;
    let mut last_id = 0_i64;
    loop {
        let occurrences = {
            let mut statement = transaction.prepare(
                "SELECT o.id,o.file_id,o.name,o.start,o.end,o.role,o.candidates,o.provenance FROM occurrences o JOIN files f ON f.id=o.file_id WHERE o.id>?1 AND f.language='php' AND ((o.role='type' AND o.provenance='php_fqcn') OR (o.role='import' AND o.provenance='php_import')) ORDER BY o.id LIMIT ?2",
            )?;
            let mut rows = statement.query(params![last_id, OCCURRENCE_BATCH as i64])?;
            let mut batch = Vec::with_capacity(OCCURRENCE_BATCH);
            while let Some(row) = rows.next()? {
                batch.push(PhpNameOccurrence {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    name: row.get(2)?,
                    start: row.get(3)?,
                    end: row.get(4)?,
                    role: row.get(5)?,
                    candidates: row.get(6)?,
                    provenance: row.get(7)?,
                });
            }
            batch
        };
        if occurrences.is_empty() {
            break;
        }

        for occurrence in occurrences {
            last_id = occurrence.id;
            if occurrence.provenance == "xenforo_generated_placeholder"
                || is_xfcp_name(&occurrence.name)
            {
                continue;
            }
            let Some(key) = normalized_fqcn(&occurrence.name) else {
                continue;
            };
            let indexed = class_index.candidates(&key);
            if indexed.is_empty() {
                continue;
            }

            let mut all_candidates = serde_json::from_str::<Vec<i64>>(&occurrence.candidates)?;
            let mut cross_file_candidates =
                Vec::with_capacity(indexed.len().min(MAX_CANDIDATES + 1));
            for candidate in indexed
                .iter()
                .filter(|candidate| candidate.file_id != occurrence.file_id)
                .take(MAX_CANDIDATES + 1)
            {
                cross_file_candidates.push(candidate.definition_id);
                all_candidates.push(candidate.definition_id);
            }
            all_candidates.sort_unstable();
            all_candidates.dedup();
            let truncated = cross_file_candidates.len() > MAX_CANDIDATES
                || all_candidates.len() > MAX_CANDIDATES;
            all_candidates.truncate(MAX_CANDIDATES);
            if all_candidates.is_empty() && !truncated {
                continue;
            }

            let mut provenance = occurrence.provenance;
            provenance.push_str(";php_fqcn_candidate_lookup");
            if truncated {
                provenance.push_str(";php_candidates_truncated");
            }
            transaction.execute(
                "UPDATE occurrences SET candidates=?1,provenance=?2 WHERE id=?3",
                params![
                    serde_json::to_string(&all_candidates)?,
                    provenance,
                    occurrence.id
                ],
            )?;

            let owner: Option<i64> = transaction
                .query_row(
                    "SELECT id FROM definitions WHERE file_id=?1 AND start<=?2 AND end>=?3 ORDER BY (end-start),id LIMIT 1",
                    params![occurrence.file_id, occurrence.start, occurrence.end],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(owner) = owner else {
                continue;
            };
            let relationship_kind = if occurrence.role == "import" {
                "php_import_candidate"
            } else {
                "php_type_candidate"
            };
            for candidate in cross_file_candidates.into_iter().take(MAX_CANDIDATES) {
                transaction.execute(
                    "INSERT INTO relationships(source,target,kind,file_id,start,end,provenance) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        owner,
                        candidate,
                        relationship_kind,
                        occurrence.file_id,
                        occurrence.start,
                        occurrence.end,
                        provenance
                    ],
                )?;
            }
        }
    }

    transaction.commit()?;
    Ok(())
}

fn definition_fqcn(namespace: Option<&str>, name: &str) -> Option<String> {
    let name = name.trim_start_matches('\\');
    if name.is_empty() {
        return None;
    }
    if name.contains('\\') {
        return Some(name.to_owned());
    }
    let namespace = namespace
        .map(|namespace| namespace.trim_start_matches('\\'))
        .filter(|namespace| !namespace.is_empty());
    Some(match namespace {
        Some(namespace) => format!("{namespace}\\{name}"),
        None => name.to_owned(),
    })
}

fn normalized_fqcn(name: &str) -> Option<String> {
    let name = name.trim_start_matches('\\');
    if name.is_empty() {
        return None;
    }
    Some(name.to_ascii_lowercase())
}

fn is_xfcp_name(name: &str) -> bool {
    name.split('\\').any(|part| {
        part.get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("XFCP_"))
    })
}
