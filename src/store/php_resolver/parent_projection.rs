use super::hierarchy_index::PhpHierarchyIndex;
use super::parent_lookup;
use crate::extract::php_markers::{self, Marker};
use crate::store::inheritance::DerivedBudget;
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, Statement, params};

const MAX_PARENT_CALL_MARKERS: usize = 200_000;
const MAX_PARENT_CALL_CANDIDATES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ParentCallMarker {
    caller_method_id: i64,
    owner_class_id: i64,
    file_id: i64,
    start: i64,
    end: i64,
}

struct IndexedCall {
    occurrence_id: i64,
    method_name: String,
}

struct ParentCallProjection {
    occurrence_id: i64,
    candidate_ids: Vec<i64>,
    occurrence_provenance: String,
    relationships: Vec<DerivedRelationship>,
}

struct DerivedRelationship {
    source: i64,
    target: Option<i64>,
    kind: &'static str,
    file_id: i64,
    start: i64,
    end: i64,
    provenance: String,
}

/// Projects literal `parent::method()` markers into bounded navigation candidates.
pub(super) fn resolve_parent_calls(
    conn: &Connection,
    hierarchy: &PhpHierarchyIndex,
    budget: &mut DerivedBudget,
) -> Result<()> {
    let markers = load_parent_call_markers(conn)?;
    let mut occurrence_query = conn.prepare(
        "SELECT id,name,target FROM occurrences
         WHERE file_id=?1 AND start=?2 AND end=?3 AND role='call'
         ORDER BY id",
    )?;
    let mut projections = Vec::with_capacity(markers.len());

    for marker in markers {
        let call = indexed_call(
            &mut occurrence_query,
            marker.file_id,
            marker.start,
            marker.end,
        )?;
        let lookup = parent_lookup::lookup_parent_method(
            hierarchy,
            marker.owner_class_id,
            &call.method_name,
        );
        projections.push(project_call(marker, call, lookup, budget)?);
    }
    drop(occurrence_query);

    let mut update_occurrence = conn.prepare(
        "UPDATE occurrences SET candidates=?1,provenance=?2
         WHERE id=?3 AND role='call' AND target IS NULL",
    )?;
    let mut insert_relationship = conn.prepare(
        "INSERT INTO relationships(source,target,kind,file_id,start,end,provenance)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;

    for projection in projections {
        let candidates = serde_json::to_string(&projection.candidate_ids)?;
        ensure!(
            update_occurrence.execute(params![
                candidates,
                projection.occurrence_provenance,
                projection.occurrence_id
            ])? == 1,
            "PHP parent-call occurrence changed during projection"
        );
        for relationship in projection.relationships {
            insert_relationship.execute(params![
                relationship.source,
                relationship.target,
                relationship.kind,
                relationship.file_id,
                relationship.start,
                relationship.end,
                relationship.provenance,
            ])?;
        }
    }
    Ok(())
}

fn load_parent_call_markers(conn: &Connection) -> Result<Vec<ParentCallMarker>> {
    let mut statement = conn.prepare(
        "SELECT source,target,file_id,start,end,kind,provenance
         FROM relationships WHERE kind=?1 ORDER BY file_id,start,source",
    )?;
    let mut rows = statement.query([php_markers::MARKER_KIND])?;
    let mut markers = Vec::new();
    while let Some(row) = rows.next()? {
        let source: i64 = row.get(0)?;
        let target: Option<i64> = row.get(1)?;
        let file_id: i64 = row.get(2)?;
        let start: i64 = row.get(3)?;
        let end: i64 = row.get(4)?;
        let kind: String = row.get(5)?;
        let provenance: String = row.get(6)?;
        let Some(decoded) =
            php_markers::decode_fields(&kind, &provenance, source, target, start, end)
        else {
            continue;
        };
        if decoded.marker != Marker::ParentCall {
            continue;
        }
        let Some(owner_class_id) = decoded.target else {
            bail!("PHP parent-call marker has no owner class");
        };
        ensure!(
            markers.len() < MAX_PARENT_CALL_MARKERS,
            "PHP parent-call marker limit exceeded"
        );
        markers.push(ParentCallMarker {
            caller_method_id: decoded.source,
            owner_class_id,
            file_id,
            start: decoded.start,
            end: decoded.end,
        });
    }
    markers.sort_unstable();
    markers.dedup();
    Ok(markers)
}

fn indexed_call(
    statement: &mut Statement<'_>,
    file_id: i64,
    start: i64,
    end: i64,
) -> Result<IndexedCall> {
    let mut rows = statement.query(params![file_id, start, end])?;
    let Some(row) = rows.next()? else {
        bail!("PHP parent-call marker has no indexed call occurrence");
    };
    let occurrence_id: i64 = row.get(0)?;
    let method_name: String = row.get(1)?;
    let target: Option<i64> = row.get(2)?;
    ensure!(
        target.is_none(),
        "PHP parent-call occurrence has a concrete target"
    );
    ensure!(
        !method_name.is_empty(),
        "PHP parent-call occurrence has no token name"
    );
    ensure!(
        rows.next()?.is_none(),
        "PHP parent-call marker matches multiple indexed call occurrences"
    );
    Ok(IndexedCall {
        occurrence_id,
        method_name,
    })
}

fn project_call(
    marker: ParentCallMarker,
    call: IndexedCall,
    lookup: parent_lookup::ParentLookup,
    budget: &mut DerivedBudget,
) -> Result<ParentCallProjection> {
    let parent_lookup::ParentLookup {
        mut candidates,
        issues,
        truncated,
    } = lookup;
    candidates.sort_unstable();
    candidates.dedup();
    let candidate_limit = candidates.len() > MAX_PARENT_CALL_CANDIDATES;
    candidates.truncate(MAX_PARENT_CALL_CANDIDATES);

    let mut issue_codes: Vec<(Option<i64>, String)> = issues
        .into_iter()
        .map(|issue| (issue.class_id, issue.kind.code().to_owned()))
        .collect();
    let has_limit_issue = issue_codes
        .iter()
        .any(|(_, code)| code == "candidate_limit" || code == "visit_limit");
    if candidate_limit || (truncated && !has_limit_issue) {
        issue_codes.push((None, "candidate_limit".to_owned()));
    }
    if candidates.is_empty() && issue_codes.is_empty() {
        issue_codes.push((Some(marker.owner_class_id), "missing_method".to_owned()));
    }
    issue_codes.sort_unstable();
    issue_codes.dedup();

    let limitations = issue_codes
        .iter()
        .map(|(_, code)| code.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let mut occurrence_provenance = if candidates.is_empty() {
        "php_parent_call_unresolved".to_owned()
    } else {
        "php_parent_call_candidate".to_owned()
    };
    occurrence_provenance.push_str(";candidate_only");
    if !limitations.is_empty() {
        occurrence_provenance.push_str(";limitations=");
        occurrence_provenance.push_str(&limitations);
    }

    let mut relationships = Vec::with_capacity(candidates.len() + issue_codes.len());
    for candidate_id in &candidates {
        budget.charge(1)?;
        budget.charge(1)?;
        relationships.push(DerivedRelationship {
            source: marker.caller_method_id,
            target: Some(*candidate_id),
            kind: "php_parent_call_candidate",
            file_id: marker.file_id,
            start: marker.start,
            end: marker.end,
            provenance: occurrence_provenance.clone(),
        });
    }
    for (class_id, code) in issue_codes {
        budget.charge(1)?;
        relationships.push(DerivedRelationship {
            source: marker.caller_method_id,
            target: None,
            kind: "inheritance_issue",
            file_id: marker.file_id,
            start: marker.start,
            end: marker.end,
            provenance: format!(
                "php_parent_call;incomplete_inheritance={code};class_id={}",
                class_id.map_or_else(|| "unknown".to_owned(), |id| id.to_string())
            ),
        });
    }

    Ok(ParentCallProjection {
        occurrence_id: call.occurrence_id,
        candidate_ids: candidates,
        occurrence_provenance,
        relationships,
    })
}
