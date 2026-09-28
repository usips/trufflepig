use super::hierarchy_index::{
    PhpClassFact, PhpClassKind, PhpClassRole, PhpHierarchyIndex, PhpMethodFact, PhpParentCandidate,
    PhpParentKind, PhpParentStatus, PhpVisibility,
};
use crate::extract::php_markers::{self, ClassRole, DecodedMarker, Marker, ParentKind, Visibility};
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;

mod projection;
pub(super) use projection::project;

const MAX_PARENT_CANDIDATES: usize = 64;

struct PhpDefinitionRow {
    id: i64,
    file_id: i64,
    name: String,
    kind: PhpClassKind,
    definition_kind: String,
    container: Option<String>,
    file_path: String,
    file_complete: bool,
}

struct StoredMarker {
    file_complete: bool,
    decoded: DecodedMarker,
    raw_evidence: Option<Vec<u8>>,
}

pub(super) fn build_index(
    conn: &Connection,
    canonicalize: impl Fn(&str) -> Option<String>,
) -> Result<PhpHierarchyIndex> {
    let definitions = definitions(conn)?;
    let markers = markers(conn)?;

    let mut classes = Vec::with_capacity(definitions.len());
    let mut class_completeness = HashMap::with_capacity(definitions.len());
    let mut trait_uses = HashMap::<i64, (bool, bool)>::new();
    let mut method_markers = HashMap::<i64, Vec<(i64, Visibility, bool, bool)>>::new();
    let mut parent_markers = Vec::new();
    let mut class_markers = HashMap::<i64, Vec<(ClassRole, bool)>>::new();

    for stored in &markers {
        match &stored.decoded.marker {
            Marker::Class { role, conditional } => {
                class_markers
                    .entry(stored.decoded.source)
                    .or_default()
                    .push((*role, *conditional));
            }
            Marker::TraitUse { conditional } => {
                let state = trait_uses.entry(stored.decoded.source).or_default();
                state.0 = true;
                state.1 |= *conditional;
            }
            Marker::Method {
                visibility,
                abstract_method,
                conditional,
            } => {
                if let Some(owner) = stored.decoded.target {
                    method_markers
                        .entry(stored.decoded.source)
                        .or_default()
                        .push((owner, *visibility, *abstract_method, *conditional));
                }
            }
            Marker::Parent { .. } => parent_markers.push(stored),
            Marker::ParentCall => {}
        }
    }

    let mut definitions_by_id = HashMap::with_capacity(definitions.len());
    for definition in &definitions {
        definitions_by_id.insert(definition.id, definition);
    }
    for definition in &definitions {
        if definition.kind == PhpClassKind::Unknown {
            continue;
        }
        let Some(fqcn) = definition_fqcn(definition.container.as_deref(), &definition.name) else {
            continue;
        };
        let marker = class_markers.get(&definition.id);
        let (role, conditional, marker_complete) = match marker.map(Vec::as_slice) {
            Some([(role, conditional)]) => (php_class_role(*role), *conditional, true),
            _ => (PhpClassRole::Unknown, false, false),
        };
        let complete = definition.file_complete && marker_complete;
        let canonical_fqcn = canonicalize(&fqcn);
        let (has_trait_use, trait_use_conditional) =
            trait_uses.get(&definition.id).copied().unwrap_or_default();
        class_completeness.insert(definition.id, (complete, conditional, role));
        classes.push(PhpClassFact {
            definition_id: definition.id,
            file_id: definition.file_id,
            file_path: definition.file_path.clone(),
            fqcn,
            canonical_fqcn,
            kind: definition.kind,
            role,
            complete,
            conditional,
            has_trait_use,
            trait_use_conditional,
        });
    }

    let mut methods = Vec::with_capacity(method_markers.len());
    for definition in &definitions {
        if definition.definition_kind != "method" {
            continue;
        }
        let Some(facts) = method_markers.get(&definition.id) else {
            continue;
        };
        for (owner_class_id, visibility, abstract_method, conditional) in facts {
            if !definitions_by_id.contains_key(owner_class_id) {
                continue;
            }
            let owner_complete = class_completeness
                .get(owner_class_id)
                .is_some_and(|(complete, _, _)| *complete);
            methods.push(PhpMethodFact {
                definition_id: definition.id,
                owner_class_id: *owner_class_id,
                file_id: definition.file_id,
                file_path: definition.file_path.clone(),
                name: definition.name.clone(),
                visibility: php_visibility(*visibility),
                abstract_method: *abstract_method,
                complete: definition.file_complete && owner_complete,
                conditional: *conditional,
            });
        }
    }

    let mut index = PhpHierarchyIndex::from_facts(classes, methods, Vec::new());
    for stored in parent_markers {
        let Marker::Parent {
            kind,
            conditional,
            resolved_name,
        } = &stored.decoded.marker
        else {
            continue;
        };
        let kind = *kind;
        let conditional = *conditional;
        let Some(parent_fqcn) = resolved_name.as_deref() else {
            let declared_name = raw_name(stored.raw_evidence.as_deref(), None);
            index.add_parent_candidate(PhpParentCandidate {
                child_class_id: stored.decoded.source,
                declared_name,
                resolved_name: None,
                canonical_name: None,
                evidence_start: stored.decoded.start,
                evidence_end: stored.decoded.end,
                kind: php_parent_kind(kind),
                candidate_class_ids: Vec::new(),
                status: if kind == ParentKind::Unknown {
                    PhpParentStatus::Unsupported
                } else {
                    PhpParentStatus::Incomplete
                },
                conditional,
                execute_order: None,
                tie_group: None,
            });
            continue;
        };

        let canonical = match kind {
            ParentKind::Ordinary => Some(parent_fqcn.to_owned()),
            ParentKind::Proxy | ParentKind::Unknown => None,
        };
        let candidates = if kind == ParentKind::Ordinary {
            index
                .raw_class_candidates(parent_fqcn)
                .map(|class| class.definition_id)
                .take(MAX_PARENT_CANDIDATES + 1)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let candidates_truncated = candidates.len() > MAX_PARENT_CANDIDATES;
        let mut candidate_class_ids = candidates;
        candidate_class_ids.truncate(MAX_PARENT_CANDIDATES);
        let source_completeness = class_completeness
            .get(&stored.decoded.source)
            .copied()
            .unwrap_or((false, false, PhpClassRole::Unknown));
        let status = parent_status(
            kind,
            conditional,
            stored.file_complete,
            source_completeness.0,
            source_completeness.1,
            source_completeness.2,
            match kind {
                ParentKind::Ordinary => canonical.is_some(),
                ParentKind::Proxy => true,
                ParentKind::Unknown => false,
            },
            candidates_truncated,
            &candidate_class_ids,
            &index,
        );
        index.add_parent_candidate(PhpParentCandidate {
            child_class_id: stored.decoded.source,
            declared_name: raw_name(stored.raw_evidence.as_deref(), Some(parent_fqcn)),
            resolved_name: Some(parent_fqcn.to_owned()),
            canonical_name: canonical,
            evidence_start: stored.decoded.start,
            evidence_end: stored.decoded.end,
            kind: php_parent_kind(kind),
            candidate_class_ids,
            status,
            conditional,
            execute_order: None,
            tie_group: None,
        });
    }
    Ok(index)
}

fn definitions(conn: &Connection) -> Result<Vec<PhpDefinitionRow>> {
    let mut statement = conn.prepare(
        "SELECT d.id,d.file_id,d.name,d.kind,d.container,f.path,f.status FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.language='php' AND d.kind IN ('class','interface','trait','enum','method') ORDER BY d.id",
    )?;
    let mut rows = statement.query([])?;
    let mut definitions = Vec::new();
    while let Some(row) = rows.next()? {
        let kind: String = row.get(3)?;
        definitions.push(PhpDefinitionRow {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            kind: php_class_kind(&kind),
            definition_kind: kind,
            container: row.get(4)?,
            file_path: row.get(5)?,
            file_complete: row.get::<_, String>(6)? == "complete",
        });
    }
    Ok(definitions)
}

fn markers(conn: &Connection) -> Result<Vec<StoredMarker>> {
    let mut statement = conn.prepare(
        "SELECT r.source,r.target,r.start,r.end,r.provenance,f.status,CASE WHEN instr(r.provenance,'php_fact_v1:parent:')=1 THEN substr(c.bytes,r.start+1,r.end-r.start) END FROM relationships r JOIN files f ON f.id=r.file_id LEFT JOIN contents c ON c.revision=f.revision WHERE f.language='php' AND r.kind=?1 ORDER BY r.file_id,r.source,r.start",
    )?;
    let mut rows = statement.query(params![php_markers::MARKER_KIND])?;
    let mut markers = Vec::new();
    while let Some(row) = rows.next()? {
        let source: i64 = row.get(0)?;
        let target: Option<i64> = row.get(1)?;
        let start: i64 = row.get(2)?;
        let end: i64 = row.get(3)?;
        let provenance: String = row.get(4)?;
        let Some(decoded) = php_markers::decode_fields(
            php_markers::MARKER_KIND,
            &provenance,
            source,
            target,
            start,
            end,
        ) else {
            continue;
        };
        markers.push(StoredMarker {
            file_complete: row.get::<_, String>(5)? == "complete",
            decoded,
            raw_evidence: row.get(6)?,
        });
    }
    Ok(markers)
}

fn parent_status(
    kind: ParentKind,
    conditional: bool,
    marker_complete: bool,
    source_class_complete: bool,
    source_class_conditional: bool,
    source_class_role: PhpClassRole,
    has_canonical_name: bool,
    candidates_truncated: bool,
    candidate_class_ids: &[i64],
    index: &PhpHierarchyIndex,
) -> PhpParentStatus {
    if kind == ParentKind::Unknown {
        return PhpParentStatus::Unsupported;
    }
    if conditional || source_class_conditional {
        return PhpParentStatus::Conditional;
    }
    if !marker_complete || !source_class_complete {
        return PhpParentStatus::Incomplete;
    }
    if !has_canonical_name {
        return PhpParentStatus::Unsupported;
    }
    if source_class_role == PhpClassRole::Unknown {
        return PhpParentStatus::Unsupported;
    }
    if candidates_truncated {
        return PhpParentStatus::Incomplete;
    }
    if candidate_class_ids.is_empty() {
        return PhpParentStatus::Missing;
    }
    if candidate_class_ids.len() > 1 {
        return PhpParentStatus::Ambiguous;
    }
    let Some(parent_class) = index.class_fact(candidate_class_ids[0]) else {
        return PhpParentStatus::Incomplete;
    };
    if parent_class.conditional {
        return PhpParentStatus::Conditional;
    }
    if !parent_class.complete {
        return PhpParentStatus::Incomplete;
    }
    if parent_class.role == PhpClassRole::Unknown {
        return PhpParentStatus::Unsupported;
    }
    PhpParentStatus::Candidates
}

fn raw_name(raw: Option<&[u8]>, fallback: Option<&str>) -> String {
    raw.map(String::from_utf8_lossy)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| fallback.map(|value| value.trim_start_matches('\\').to_owned()))
        .unwrap_or_default()
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
        .map(|value| value.trim_start_matches('\\'))
        .filter(|value| !value.is_empty());
    Some(match namespace {
        Some(namespace) => format!("{namespace}\\{name}"),
        None => name.to_owned(),
    })
}

fn php_class_kind(kind: &str) -> PhpClassKind {
    match kind {
        "class" => PhpClassKind::Class,
        "interface" => PhpClassKind::Interface,
        "trait" => PhpClassKind::Trait,
        "enum" => PhpClassKind::Enum,
        _ => PhpClassKind::Unknown,
    }
}

fn php_class_role(role: ClassRole) -> PhpClassRole {
    match role {
        ClassRole::Ordinary => PhpClassRole::Ordinary,
        ClassRole::Proxy => PhpClassRole::Proxy,
        ClassRole::Unknown => PhpClassRole::Unknown,
    }
}

fn php_parent_kind(kind: ParentKind) -> PhpParentKind {
    match kind {
        ParentKind::Ordinary => PhpParentKind::Extends,
        ParentKind::Proxy => PhpParentKind::XfcpProxy,
        ParentKind::Unknown => PhpParentKind::Unknown,
    }
}

fn php_visibility(visibility: Visibility) -> PhpVisibility {
    match visibility {
        Visibility::Public => PhpVisibility::Public,
        Visibility::Protected => PhpVisibility::Protected,
        Visibility::Private => PhpVisibility::Private,
        Visibility::Unknown => PhpVisibility::Unknown,
    }
}
