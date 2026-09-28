use super::super::MAX_CANDIDATES;
use super::proxy_validation::{class_key, join_reasons, proxy_readiness_issues};
use super::{
    ClassTarget, ImplementationClass, ImplementationLookup, PendingRow, PreparedChainIssue,
    PreparedChainProjection, PreparedParentEdge, PreparedPlaceholderUpdate, RegistrationEvidence,
};
use crate::store::inheritance::DerivedBudget;
use crate::store::inheritance::ordered_chains::{
    ChainIssue, PredecessorCandidate, Registration, RegistrationIndex,
};
use crate::store::php_resolver::hierarchy_index::{
    PhpClassKind, PhpClassRole, PhpParentCandidate, PhpParentKind, PhpParentStatus,
};
use anyhow::{Context, Result, ensure};
use std::collections::{HashMap, HashSet};

pub(super) fn project_base_graph(
    graph: &crate::store::inheritance::ordered_chains::ChainGraph<'_, RegistrationEvidence>,
    indices: &[RegistrationIndex],
    registrations: &[Registration<RegistrationEvidence>],
    implementation_cache: &HashMap<(String, String, String), ImplementationLookup>,
    base_cache: &HashMap<String, (Vec<ClassTarget>, bool)>,
) -> Result<PreparedChainProjection> {
    let mut prepared = empty_projection();
    let mut tied_priorities = HashMap::<RegistrationIndex, i64>::new();
    for issue in &graph.issues {
        if let ChainIssue::PriorityTie {
            numeric_priority,
            registrations: tied,
            ..
        } = issue
        {
            for registration in tied {
                tied_priorities.insert(*registration, *numeric_priority);
            }
        }
    }

    let fatal_issue = graph.issues.iter().find_map(|issue| match issue {
        ChainIssue::ClassLimitExceeded { .. } => Some("class_limit_exceeded"),
        ChainIssue::CandidateLimitExceeded { .. } => Some("candidate_limit_exceeded"),
        ChainIssue::PriorityTie { .. } => None,
    });
    if let Some(reason) = fatal_issue {
        for &index in indices {
            let registration = registrations
                .get(index.0)
                .context("ordered chain returned an unknown registration")?;
            let evidence = &registration.evidence;
            let lookup = implementation_cache
                .get(&implementation_cache_key(evidence))
                .context("ordered chain implementation lookup is missing")?;
            if lookup.classes.is_empty() {
                push_registration_issue(
                    &mut prepared,
                    None,
                    evidence,
                    &format!("{};incomplete={reason}", registration_provenance(evidence)),
                );
            }
            for implementation in &lookup.classes {
                add_incomplete_registration(
                    &mut prepared,
                    implementation,
                    evidence,
                    &format!("{};incomplete={reason}", registration_provenance(evidence)),
                    None,
                )?;
            }
        }
        return Ok(prepared);
    }

    let mut targets_by_child = HashMap::<i64, ChildProjection>::new();
    let mut edge_dedup = HashSet::<(i64, i64, i64, usize, usize)>::new();
    let mut issue_dedup = HashSet::<(i64, usize, usize, String)>::new();
    for edge in &graph.edges {
        let child_registration = registrations
            .get(edge.child.0)
            .context("ordered chain child registration is missing")?;
        let child = &child_registration.evidence;
        let child_lookup = implementation_cache
            .get(&implementation_cache_key(child))
            .context("ordered chain child implementation lookup is missing")?;
        let (targets, target_truncated, target_identity_ambiguous, predecessor) =
            match edge.predecessor {
                PredecessorCandidate::Base => {
                    let (targets, truncated) = base_cache
                        .get(&class_key(edge.base))
                        .context("ordered chain base class lookup is missing")?;
                    (targets.clone(), *truncated, targets.len() > 1, None)
                }
                PredecessorCandidate::Registration(index) => {
                    let predecessor_registration = registrations
                        .get(index.0)
                        .context("ordered chain predecessor registration is missing")?;
                    let predecessor = &predecessor_registration.evidence;
                    let lookup = implementation_cache
                        .get(&implementation_cache_key(predecessor))
                        .context("ordered chain predecessor implementation lookup is missing")?;
                    (
                        lookup
                            .classes
                            .iter()
                            .filter(|class| class.proxy_matches)
                            .map(|class| ClassTarget {
                                definition_id: class.definition_id,
                                kind: class.kind,
                                role: class.role,
                                complete: class.complete,
                                conditional: class.conditional,
                            })
                            .collect(),
                        lookup.truncated,
                        lookup.classes.len() > 1,
                        Some(predecessor),
                    )
                }
            };

        let mut target_issues = Vec::<&str>::new();
        if target_identity_ambiguous {
            target_issues.push("predecessor_class_identity_ambiguous");
        }
        if target_truncated {
            target_issues.push("predecessor_candidates_truncated");
        }
        if targets.is_empty() {
            target_issues.push(match edge.predecessor {
                PredecessorCandidate::Base => "declared_base_class_missing",
                PredecessorCandidate::Registration(_) => "immediate_predecessor_class_missing",
            });
        } else if targets.len() == 1 {
            let target = &targets[0];
            if target.kind != PhpClassKind::Class || target.role != PhpClassRole::Ordinary {
                target_issues.push("predecessor_not_ordinary_class");
            }
            if target.conditional {
                target_issues.push("predecessor_class_conditional");
            } else if !target.complete {
                target_issues.push("predecessor_class_incomplete");
            }
        }

        let tie_priority =
            tied_priorities
                .get(&edge.child)
                .copied()
                .or_else(|| match edge.predecessor {
                    PredecessorCandidate::Registration(index) => {
                        tied_priorities.get(&index).copied()
                    }
                    PredecessorCandidate::Base => None,
                });

        let child_identity_ambiguous = child_lookup.classes.len() > 1 || child_lookup.truncated;
        let child_class = (child_lookup.classes.len() == 1 && !child_lookup.truncated)
            .then(|| &child_lookup.classes[0]);
        let mut reasons = target_issues.clone();
        if child_identity_ambiguous {
            reasons.push("implementation_class_identity_ambiguous");
        }
        if child_lookup.classes.is_empty() {
            reasons.push("implementation_class_missing");
        }
        if let Some(implementation) = child_class {
            let readiness = proxy_readiness_issues(implementation, &child.expected_proxy);
            reasons.extend(readiness.iter().copied());
            if implementation.kind != PhpClassKind::Class
                || implementation.role != PhpClassRole::Ordinary
            {
                reasons.push("implementation_not_ordinary_class");
            }
            if implementation.conditional {
                reasons.push("implementation_class_conditional");
            } else if !implementation.complete {
                reasons.push("implementation_class_incomplete");
            }
        }
        let mut candidate_ids = if reasons.is_empty() {
            targets
                .first()
                .map(|target| vec![target.definition_id])
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if let Some(implementation) = child_class {
            if candidate_ids.contains(&implementation.definition_id) {
                reasons.push("implementation_is_its_own_predecessor");
                candidate_ids.clear();
            }
        }
        candidate_ids.sort_unstable();
        candidate_ids.dedup();

        let mut provenance = edge_provenance(child, predecessor, tie_priority);
        if reasons.is_empty() {
            provenance.push_str(";immediate_parent_candidate");
        } else {
            provenance.push_str(";incomplete=");
            provenance.push_str(&join_reasons(&reasons));
        }

        if let Some(implementation) = child_class {
            let state = targets_by_child
                .entry(implementation.definition_id)
                .or_default();
            state.target_ids.extend(candidate_ids.iter().copied());
            state.tie_priority = state.tie_priority.or(tie_priority);
            state.incomplete |= !reasons.is_empty();
            state.conditional |=
                implementation.conditional || targets.iter().any(|target| target.conditional);

            if reasons.is_empty() {
                let target_id = candidate_ids[0];
                if edge_dedup.insert((
                    implementation.definition_id,
                    target_id,
                    child.file_id,
                    child.tag_span.start,
                    child.tag_span.end,
                )) {
                    prepared.parent_edges.push(PreparedParentEdge {
                        child_class_id: implementation.definition_id,
                        predecessor_class_id: target_id,
                        file_id: child.file_id,
                        span: child.tag_span.clone(),
                        provenance: provenance.clone(),
                    });
                }
            }
        }

        for reason in reasons {
            let identity = (
                child_class.map_or(0, |implementation| implementation.definition_id),
                child.tag_span.start,
                child.tag_span.end,
                reason.to_owned(),
            );
            if issue_dedup.insert(identity) {
                push_registration_issue(
                    &mut prepared,
                    child_class.map(|implementation| implementation.definition_id),
                    child,
                    &format!("{};incomplete={reason}", registration_provenance(child)),
                );
            }
        }
    }

    for &index in indices {
        let registration = registrations
            .get(index.0)
            .context("ordered chain registration is missing")?;
        let evidence = &registration.evidence;
        let lookup = implementation_cache
            .get(&implementation_cache_key(evidence))
            .context("ordered chain implementation lookup is missing")?;
        if lookup.classes.is_empty() {
            push_registration_issue(
                &mut prepared,
                None,
                evidence,
                &format!(
                    "{};incomplete=implementation_class_missing",
                    registration_provenance(evidence)
                ),
            );
            continue;
        }
        for implementation in &lookup.classes {
            let mut state = targets_by_child
                .remove(&implementation.definition_id)
                .unwrap_or_default();
            state.identity_ambiguous = lookup.classes.len() > 1;
            if state.identity_ambiguous || lookup.truncated {
                state.target_ids.clear();
            }
            state.target_ids.sort_unstable();
            state.target_ids.dedup();
            let candidate_limit = state.target_ids.len() > MAX_CANDIDATES;
            state.target_ids.truncate(MAX_CANDIDATES);
            let readiness = proxy_readiness_issues(implementation, &evidence.expected_proxy);
            let incomplete =
                state.incomplete || lookup.truncated || candidate_limit || !readiness.is_empty();
            let status = if state.identity_ambiguous {
                PhpParentStatus::Ambiguous
            } else if implementation.conditional || state.conditional {
                PhpParentStatus::Conditional
            } else if incomplete {
                PhpParentStatus::Incomplete
            } else if state.target_ids.is_empty() {
                PhpParentStatus::Incomplete
            } else {
                PhpParentStatus::Candidates
            };
            if candidate_limit {
                push_registration_issue(
                    &mut prepared,
                    Some(implementation.definition_id),
                    evidence,
                    &format!(
                        "{};incomplete=predecessor_candidates_truncated",
                        registration_provenance(evidence)
                    ),
                );
            }
            if state.target_ids.is_empty() && !incomplete {
                push_registration_issue(
                    &mut prepared,
                    Some(implementation.definition_id),
                    evidence,
                    &format!(
                        "{};incomplete=predecessor_class_missing",
                        registration_provenance(evidence)
                    ),
                );
            }
            let tie_priority = state
                .tie_priority
                .or_else(|| tied_priorities.get(&index).copied());
            if implementation.proxy_matches {
                prepared.hierarchy_edges.push(updated_proxy_parent(
                    implementation,
                    &evidence.expected_proxy,
                    state.target_ids.clone(),
                    status,
                    evidence.priority,
                    tie_priority,
                ));
                if let Some((occurrence_id, span)) = &implementation.proxy_occurrence {
                    prepared.placeholder_updates.push(PreparedPlaceholderUpdate {
                        occurrence_id: *occurrence_id,
                        file_id: implementation.file_id,
                        span: span.clone(),
                        target_class_ids: state.target_ids,
                        provenance: format!(
                            "xenforo_generated_placeholder;{};framework_parent_candidate;runtime_order_unverified",
                            registration_provenance(evidence)
                        ),
                    });
                }
            } else {
                add_incomplete_registration(
                    &mut prepared,
                    implementation,
                    evidence,
                    &format!(
                        "{};incomplete=proxy_parent_unverified",
                        registration_provenance(evidence)
                    ),
                    tie_priority,
                )?;
            }
        }
    }
    Ok(prepared)
}

#[derive(Default)]
struct ChildProjection {
    target_ids: Vec<i64>,
    incomplete: bool,
    conditional: bool,
    identity_ambiguous: bool,
    tie_priority: Option<i64>,
}

fn implementation_cache_key(evidence: &RegistrationEvidence) -> (String, String, String) {
    (
        evidence.implementation_raw_lookup.clone(),
        evidence.implementation_lookup.clone(),
        class_key(&evidence.expected_proxy),
    )
}

fn edge_provenance(
    child: &RegistrationEvidence,
    predecessor: Option<&RegistrationEvidence>,
    tie_priority: Option<i64>,
) -> String {
    let mut provenance = registration_provenance(child);
    if let Some(predecessor) = predecessor {
        provenance.push_str(&format!(
            ";predecessor_order={};predecessor_to_class={}",
            predecessor.priority, predecessor.implementation_name
        ));
    } else {
        provenance.push_str(";predecessor=declared_base");
    }
    if let Some(priority) = tie_priority {
        provenance.push_str(&format!(";priority_tie={priority}"));
    }
    provenance
}

fn registration_provenance(evidence: &RegistrationEvidence) -> String {
    format!(
        "xenforo_class_extensions_xml;active=1;execute_order={};addon_enabled=unknown;source_assumption=registration_and_addon_applicable_enabled;runtime_order_unverified;row_file_id={};row_index={};base={};implementation={}",
        evidence.priority,
        evidence.file_id,
        evidence.row_index,
        evidence.base_name,
        evidence.implementation_name,
    )
}

fn updated_proxy_parent(
    implementation: &ImplementationClass,
    expected_proxy: &str,
    candidates: Vec<i64>,
    status: PhpParentStatus,
    priority: i64,
    tie_priority: Option<i64>,
) -> PhpParentCandidate {
    let mut parent = implementation
        .proxy_parent
        .clone()
        .unwrap_or_else(|| PhpParentCandidate {
            child_class_id: implementation.definition_id,
            declared_name: expected_proxy.to_owned(),
            resolved_name: None,
            canonical_name: None,
            evidence_start: 0,
            evidence_end: 0,
            kind: PhpParentKind::XfcpProxy,
            candidate_class_ids: Vec::new(),
            status: PhpParentStatus::Incomplete,
            conditional: false,
            execute_order: None,
            tie_group: None,
        });
    parent.kind = PhpParentKind::XfcpProxy;
    parent.candidate_class_ids = candidates;
    parent.status = status;
    parent.execute_order = Some(priority);
    parent.tie_group = tie_priority;
    parent
}

pub(super) fn add_incomplete_proxy_edge(
    prepared: &mut PreparedChainProjection,
    implementation: &ImplementationClass,
    pending: &PendingRow<'_>,
    reason: String,
    target_class_ids: Vec<i64>,
    tie_priority: Option<i64>,
) -> Result<()> {
    let mut parent = updated_proxy_parent(
        implementation,
        &pending.expected_proxy,
        target_class_ids.clone(),
        PhpParentStatus::Incomplete,
        pending.extension.execute_order.unwrap_or_default(),
        tie_priority,
    );
    parent.conditional |= implementation.conditional;
    prepared.hierarchy_edges.push(parent);
    if implementation.proxy_matches {
        if let Some((occurrence_id, span)) = &implementation.proxy_occurrence {
            prepared
                .placeholder_updates
                .push(PreparedPlaceholderUpdate {
                    occurrence_id: *occurrence_id,
                    file_id: implementation.file_id,
                    span: span.clone(),
                    target_class_ids,
                    provenance: format!(
                        "xenforo_generated_placeholder;{};incomplete",
                        issue_provenance(pending, &reason)
                    ),
                });
        }
    }
    push_issue(
        prepared,
        Some(implementation.definition_id),
        pending,
        issue_provenance(pending, &reason),
    );
    Ok(())
}

fn add_incomplete_registration(
    prepared: &mut PreparedChainProjection,
    implementation: &ImplementationClass,
    evidence: &RegistrationEvidence,
    reason: &str,
    tie_priority: Option<i64>,
) -> Result<()> {
    prepared.hierarchy_edges.push(updated_proxy_parent(
        implementation,
        &evidence.expected_proxy,
        Vec::new(),
        PhpParentStatus::Incomplete,
        evidence.priority,
        tie_priority,
    ));
    if implementation.proxy_matches {
        if let Some((occurrence_id, span)) = &implementation.proxy_occurrence {
            prepared
                .placeholder_updates
                .push(PreparedPlaceholderUpdate {
                    occurrence_id: *occurrence_id,
                    file_id: implementation.file_id,
                    span: span.clone(),
                    target_class_ids: Vec::new(),
                    provenance: format!(
                        "xenforo_generated_placeholder;{reason};runtime_order_unverified"
                    ),
                });
        }
    }
    push_registration_issue(
        prepared,
        Some(implementation.definition_id),
        evidence,
        reason,
    );
    Ok(())
}

pub(super) fn push_issue(
    prepared: &mut PreparedChainProjection,
    child_class_id: Option<i64>,
    pending: &PendingRow<'_>,
    provenance: String,
) {
    prepared.issues.push(PreparedChainIssue {
        child_class_id,
        extension_definition_id: None,
        file_id: pending.file.file_id,
        span: pending.extension.tag_span.clone(),
        provenance: format!("xenforo_class_extensions_xml;incomplete;{provenance}"),
    });
}

fn push_registration_issue(
    prepared: &mut PreparedChainProjection,
    child_class_id: Option<i64>,
    evidence: &RegistrationEvidence,
    provenance: &str,
) {
    prepared.issues.push(PreparedChainIssue {
        child_class_id,
        extension_definition_id: None,
        file_id: evidence.file_id,
        span: evidence.tag_span.clone(),
        provenance: format!("{};incomplete", provenance),
    });
}

pub(super) fn issue_provenance(pending: &PendingRow<'_>, reason: &str) -> String {
    format!(
        "active={};execute_order={};addon_enabled=unknown;source_assumption=registration_and_addon_applicable_enabled;reason={reason}",
        pending
            .extension
            .active
            .map_or("unknown", |active| if active { "1" } else { "0" }),
        pending
            .extension
            .execute_order
            .map_or_else(|| "unknown".to_owned(), |order| order.to_string()),
    )
}

fn empty_projection() -> PreparedChainProjection {
    PreparedChainProjection {
        parent_edges: Vec::new(),
        placeholder_updates: Vec::new(),
        class_occurrence_updates: Vec::new(),
        hierarchy_edges: Vec::new(),
        issues: Vec::new(),
    }
}

pub(super) fn charge_and_append(
    target: &mut PreparedChainProjection,
    mut additions: PreparedChainProjection,
    budget: &mut DerivedBudget,
) -> Result<()> {
    let candidate_ids = additions
        .placeholder_updates
        .iter()
        .map(|update| update.target_class_ids.len())
        .sum::<usize>();
    let cost = additions
        .parent_edges
        .len()
        .saturating_add(additions.hierarchy_edges.len())
        .saturating_add(additions.issues.len())
        .saturating_add(candidate_ids);
    budget.charge(cost)?;
    target.parent_edges.append(&mut additions.parent_edges);
    target
        .placeholder_updates
        .append(&mut additions.placeholder_updates);
    target
        .hierarchy_edges
        .append(&mut additions.hierarchy_edges);
    target.issues.append(&mut additions.issues);
    Ok(())
}

pub(super) fn coalesce_placeholder_updates(prepared: &mut PreparedChainProjection) -> Result<()> {
    let mut positions = HashMap::<i64, usize>::with_capacity(prepared.placeholder_updates.len());
    let mut unique = Vec::with_capacity(prepared.placeholder_updates.len());
    for update in prepared.placeholder_updates.drain(..) {
        let Some(&position) = positions.get(&update.occurrence_id) else {
            positions.insert(update.occurrence_id, unique.len());
            unique.push(update);
            continue;
        };
        let previous = &mut unique[position];
        ensure!(
            previous.file_id == update.file_id && previous.span == update.span,
            "one XenForo placeholder occurrence maps to conflicting source evidence"
        );
        let mut previous_candidates = previous.target_class_ids.clone();
        previous_candidates.sort_unstable();
        previous_candidates.dedup();
        let mut new_candidates = update.target_class_ids;
        new_candidates.sort_unstable();
        new_candidates.dedup();
        if previous_candidates != new_candidates {
            previous_candidates.clear();
            previous
                .provenance
                .push_str(";incomplete=multiple_registration_evidence");
        }
        previous.target_class_ids = previous_candidates;
    }
    prepared.placeholder_updates = unique;
    Ok(())
}
