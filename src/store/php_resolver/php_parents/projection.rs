use super::super::hierarchy_index::{
    PhpClassRole, PhpHierarchyIndex, PhpParentKind, PhpParentStatus,
};
use crate::store::inheritance::DerivedBudget;
use anyhow::Result;
use rusqlite::{Connection, params};

const MAX_PHP_PARENT_CANDIDATES: usize = 64;

struct ParentRelationship {
    source: i64,
    target: Option<i64>,
    kind: &'static str,
    file_id: i64,
    start: i64,
    end: i64,
    provenance: &'static str,
}

/// Publishes ordinary PHP `extends` observations as candidate relationships.
pub(in crate::store::php_resolver) fn project(
    conn: &Connection,
    index: &PhpHierarchyIndex,
    budget: &mut DerivedBudget,
) -> Result<()> {
    let mut staged = Vec::new();
    for child in index.classes() {
        if child.role == PhpClassRole::Proxy {
            continue;
        }
        for parent in index.parent_candidates(child.definition_id) {
            if parent.kind == PhpParentKind::XfcpProxy {
                continue;
            }
            if parent.kind != PhpParentKind::Extends {
                stage_issue(
                    &mut staged,
                    child.definition_id,
                    child.file_id,
                    parent,
                    "unsupported",
                    budget,
                )?;
                continue;
            }

            let mut targets = parent.candidate_class_ids.clone();
            targets.sort_unstable();
            targets.dedup();
            if parent.status == PhpParentStatus::Candidates
                && !parent.conditional
                && !targets.is_empty()
                && targets.len() <= MAX_PHP_PARENT_CANDIDATES
            {
                for target in targets {
                    budget.charge(1)?;
                    staged.push(ParentRelationship {
                        source: child.definition_id,
                        target: Some(target),
                        kind: "php_extends_candidate",
                        file_id: child.file_id,
                        start: parent.evidence_start,
                        end: parent.evidence_end,
                        provenance: "php_parent_source;candidate_only",
                    });
                }
            } else {
                let code = issue_code(parent.status, parent.conditional, targets.is_empty());
                stage_issue(
                    &mut staged,
                    child.definition_id,
                    child.file_id,
                    parent,
                    code,
                    budget,
                )?;
            }
        }
    }

    let mut statement = conn.prepare(
        "INSERT INTO relationships(source,target,kind,file_id,start,end,provenance)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    for relationship in staged {
        statement.execute(params![
            relationship.source,
            relationship.target,
            relationship.kind,
            relationship.file_id,
            relationship.start,
            relationship.end,
            relationship.provenance,
        ])?;
    }
    Ok(())
}

fn stage_issue(
    staged: &mut Vec<ParentRelationship>,
    child_id: i64,
    file_id: i64,
    parent: &super::super::hierarchy_index::PhpParentCandidate,
    code: &'static str,
    budget: &mut DerivedBudget,
) -> Result<()> {
    budget.charge(1)?;
    staged.push(ParentRelationship {
        source: child_id,
        target: None,
        kind: "inheritance_issue",
        file_id,
        start: parent.evidence_start,
        end: parent.evidence_end,
        provenance: match code {
            "missing" => "php_parent_issue:missing",
            "ambiguous" => "php_parent_issue:ambiguous",
            "conditional" => "php_parent_issue:conditional",
            "incomplete" => "php_parent_issue:incomplete",
            _ => "php_parent_issue:unsupported",
        },
    });
    Ok(())
}

fn issue_code(status: PhpParentStatus, conditional: bool, no_candidates: bool) -> &'static str {
    if conditional {
        return "conditional";
    }
    match status {
        PhpParentStatus::Candidates if no_candidates => "missing",
        PhpParentStatus::Candidates => "incomplete",
        PhpParentStatus::Missing => "missing",
        PhpParentStatus::Ambiguous => "ambiguous",
        PhpParentStatus::Conditional => "conditional",
        PhpParentStatus::Incomplete => "incomplete",
        PhpParentStatus::Unsupported => "unsupported",
    }
}
