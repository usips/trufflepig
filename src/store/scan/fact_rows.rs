//! Inserts one file's extracted facts into a staged or in-memory index.
use crate::extract::Extraction;
use anyhow::Result;
use rusqlite::{Transaction, params};

/// Inserts one file's extracted facts with statements compiled once per connection.
pub(in crate::store) fn insert_facts(
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
pub(in crate::store) fn insert_module_definition(
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
