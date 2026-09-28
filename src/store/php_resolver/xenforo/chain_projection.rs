use super::chain_adapter::PreparedChainProjection;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use std::collections::HashSet;

const MAX_CANDIDATES: usize = 64;

/// Publishes prepared XenForo predecessor edges and placeholder candidates.
pub(super) fn project(conn: &Connection, prepared: &PreparedChainProjection) -> Result<()> {
    let mut insert_relationship = conn.prepare(
        "INSERT INTO relationships(source,target,kind,file_id,start,end,provenance) VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    for edge in &prepared.parent_edges {
        ensure!(
            edge.child_class_id > 0
                && edge.predecessor_class_id > 0
                && edge.file_id > 0
                && edge.span.start < edge.span.end,
            "invalid source in prepared XenForo parent edge"
        );
        insert_relationship.execute(params![
            edge.child_class_id,
            edge.predecessor_class_id,
            "framework_parent_candidate",
            edge.file_id,
            i64::try_from(edge.span.start)?,
            i64::try_from(edge.span.end)?,
            &edge.provenance,
        ])?;
    }

    let mut find_extension_definition = conn.prepare(
        "SELECT count(*),min(id) FROM definitions WHERE file_id=?1 AND kind='xenforo_class_extension' AND start=?2 AND end=?3",
    )?;
    for issue in &prepared.issues {
        ensure!(
            issue.file_id > 0 && issue.span.start < issue.span.end,
            "invalid evidence in prepared XenForo inheritance issue"
        );
        let start = i64::try_from(issue.span.start)?;
        let end = i64::try_from(issue.span.end)?;
        let source = match issue.child_class_id {
            Some(child) => child,
            None => {
                let (count, extension): (i64, Option<i64>) = find_extension_definition
                    .query_row(params![issue.file_id, start, end], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?;
                ensure!(
                    count == 1,
                    "XenForo inheritance issue has no unique XML declaration"
                );
                let extension =
                    extension.context("XenForo inheritance issue declaration has no definition")?;
                if let Some(expected) = issue.extension_definition_id {
                    ensure!(
                        expected == extension,
                        "XenForo inheritance issue definition does not match its XML span"
                    );
                }
                extension
            }
        };
        ensure!(source > 0, "invalid source ID in XenForo inheritance issue");
        insert_relationship.execute(params![
            source,
            None::<i64>,
            "inheritance_issue",
            issue.file_id,
            start,
            end,
            &issue.provenance,
        ])?;
    }
    drop(insert_relationship);

    project_class_occurrence_updates(conn, prepared)?;

    let mut update_placeholder = conn.prepare(
        "UPDATE occurrences SET target=NULL,candidates=?1,provenance=provenance || ';' || ?2 WHERE id=?3 AND file_id=?4 AND start=?5 AND end=?6 AND role='type'",
    )?;
    let mut seen_occurrences = HashSet::with_capacity(prepared.placeholder_updates.len());
    for update in &prepared.placeholder_updates {
        ensure!(
            seen_occurrences.insert(update.occurrence_id),
            "duplicate XenForo placeholder update"
        );
        ensure!(
            update.occurrence_id > 0,
            "invalid placeholder occurrence ID"
        );
        ensure!(update.file_id > 0, "invalid placeholder source file ID");
        ensure!(
            update.span.start < update.span.end,
            "invalid placeholder source span"
        );
        ensure!(
            !update.provenance.is_empty(),
            "placeholder provenance is empty"
        );

        let mut candidates = update.target_class_ids.clone();
        candidates.sort_unstable();
        candidates.dedup();
        ensure!(
            candidates.len() <= MAX_CANDIDATES,
            "XenForo placeholder candidate limit exceeded"
        );
        ensure!(
            candidates.iter().all(|candidate| *candidate > 0),
            "invalid class ID in XenForo placeholder candidates"
        );

        let changed = update_placeholder.execute(params![
            serde_json::to_string(&candidates)?,
            &update.provenance,
            update.occurrence_id,
            update.file_id,
            i64::try_from(update.span.start)?,
            i64::try_from(update.span.end)?,
        ])?;
        ensure!(
            changed == 1,
            "prepared XenForo placeholder occurrence is missing or changed"
        );
    }
    Ok(())
}

fn project_class_occurrence_updates(
    conn: &Connection,
    prepared: &PreparedChainProjection,
) -> Result<()> {
    let mut find_extension_definition = conn.prepare(
        "SELECT count(*),min(id) FROM definitions WHERE file_id=?1 AND kind='xenforo_class_extension' AND start=?2 AND end=?3",
    )?;
    let mut update_occurrence = conn.prepare(
        "UPDATE occurrences SET target=NULL,candidates=?1,provenance=?2
         WHERE file_id=?3 AND start=?4 AND end=?5 AND role=?6 AND name=?7",
    )?;
    let mut delete_relationships = conn.prepare(
        "DELETE FROM relationships WHERE source=?1 AND file_id=?2 AND start=?3 AND end=?4 AND kind IN (?5,?6)",
    )?;
    let mut insert_relationship = conn.prepare(
        "INSERT INTO relationships(source,target,kind,file_id,start,end,provenance)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    let mut seen = HashSet::with_capacity(prepared.class_occurrence_updates.len());

    for update in &prepared.class_occurrence_updates {
        ensure!(
            update.file_id > 0 && update.span.start < update.span.end,
            "invalid XenForo class occurrence update evidence"
        );
        ensure!(
            seen.insert((
                update.file_id,
                update.span.start,
                update.span.end,
                update.role
            )),
            "duplicate XenForo class occurrence update"
        );

        let mut candidates = update.class_ids.clone();
        candidates.sort_unstable();
        candidates.dedup();
        ensure!(
            candidates.len() <= MAX_CANDIDATES && candidates.iter().all(|candidate| *candidate > 0),
            "invalid XenForo class occurrence candidates"
        );

        let changed = update_occurrence.execute(params![
            serde_json::to_string(&candidates)?,
            &update.provenance,
            update.file_id,
            i64::try_from(update.span.start)?,
            i64::try_from(update.span.end)?,
            update.role,
            &update.name,
        ])?;
        ensure!(
            changed == 1,
            "prepared XenForo class occurrence is missing or changed"
        );

        let start = i64::try_from(update.tag_span.start)?;
        let end = i64::try_from(update.tag_span.end)?;
        let (count, extension): (i64, Option<i64>) = find_extension_definition
            .query_row(params![update.file_id, start, end], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        ensure!(
            count == 1,
            "XenForo class occurrence has no unique extension declaration"
        );
        let extension = extension.context("XenForo extension declaration has no definition")?;
        let (candidate_kind, unresolved_kind) = if update.role == "xenforo_extension_base" {
            (
                "xenforo_class_extension_candidate",
                "xenforo_class_extension",
            )
        } else {
            ensure!(
                update.role == "xenforo_extension_implementation",
                "unsupported XenForo class occurrence role"
            );
            (
                "xenforo_class_extension_implementation_candidate",
                "xenforo_class_extension_implementation",
            )
        };
        delete_relationships.execute(params![
            extension,
            update.file_id,
            start,
            end,
            candidate_kind,
            unresolved_kind,
        ])?;

        let (kind, targets): (&str, Vec<Option<i64>>) = if candidates.is_empty() {
            (unresolved_kind, vec![None])
        } else {
            (candidate_kind, candidates.into_iter().map(Some).collect())
        };
        for target in targets {
            insert_relationship.execute(params![
                extension,
                target,
                kind,
                update.file_id,
                start,
                end,
                &update.provenance,
            ])?;
        }
    }
    Ok(())
}
