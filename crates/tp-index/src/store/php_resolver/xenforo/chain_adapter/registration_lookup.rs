use super::super::alias_names::canonicalize_class_name;
use super::super::alias_source::AliasSource;
use super::super::{ExtensionFile, MAX_CANDIDATES, MAX_METADATA_FACTS};
use super::chain_facts::{
    add_incomplete_proxy_edge, charge_and_append, coalesce_placeholder_updates, issue_provenance,
    project_base_graph, push_issue,
};
use super::proxy_validation::{
    class_key, expected_proxy_name, join_reasons, proxy_readiness_issues,
};
use super::{
    ClassTarget, ImplementationClass, ImplementationLookup, PendingRow, PreparedChainProjection,
    PreparedClassOccurrenceUpdate, RegistrationEvidence,
};
use crate::store::inheritance::DerivedBudget;
use crate::store::inheritance::ordered_chains::{
    Registration, RegistrationIndex, build_chain_for_base,
};
use crate::store::php_resolver::hierarchy_index::{PhpHierarchyIndex, PhpParentKind};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

/// Converts source metadata into bounded candidate predecessors without choosing a runtime chain.
pub(in crate::store::php_resolver::xenforo) fn prepare(
    conn: &Connection,
    files: &[ExtensionFile],
    hierarchy: &PhpHierarchyIndex,
    aliases: &AliasSource,
    budget: &mut DerivedBudget,
) -> Result<PreparedChainProjection> {
    let mut prepared = PreparedChainProjection {
        parent_edges: Vec::new(),
        placeholder_updates: Vec::new(),
        class_occurrence_updates: Vec::new(),
        hierarchy_edges: Vec::new(),
        issues: Vec::new(),
    };
    let mut rows = Vec::new();
    let mut implementation_cache = HashMap::<(String, String, String), ImplementationLookup>::new();
    let mut base_cache = HashMap::<String, (Vec<ClassTarget>, bool)>::new();
    let mut source_candidate_count = 0_usize;
    let mut blocked_bases = HashSet::<String>::new();
    let mut proxy_owners = HashMap::<String, Vec<usize>>::new();

    for file in files {
        for (row_index, extension) in file.extensions.iter().enumerate() {
            if extension.active == Some(false) {
                continue;
            }
            let base = canonicalize_class_name(&extension.from_class, aliases);
            let implementation = canonicalize_class_name(&extension.to_class, aliases);
            let expected_proxy = expected_proxy_name(&extension.to_class)?;
            let mut metadata_issues = Vec::with_capacity(4);
            if extension.active.is_none() {
                metadata_issues.push("active_state_unknown");
            }
            if extension.execute_order.is_none() {
                metadata_issues.push("execute_order_unknown");
            } else if extension.execute_order.is_some_and(|order| order < 0) {
                metadata_issues.push("execute_order_invalid_negative");
            }
            if extension.duplicate {
                metadata_issues.push("duplicate_declaration");
            }
            if base.is_none() || implementation.is_none() {
                metadata_issues.push("alias_source_unresolved");
            }

            let pending_index = rows.len();
            if let Some(base_key) = base.as_deref() {
                if !metadata_issues.is_empty() {
                    blocked_bases.insert(class_key(base_key));
                }
            }
            proxy_owners
                .entry(class_key(&expected_proxy))
                .or_default()
                .push(pending_index);
            rows.push(PendingRow {
                file,
                row_index,
                extension,
                base,
                implementation,
                expected_proxy,
                metadata_issues,
            });
        }
    }

    for owners in proxy_owners.values() {
        if owners.len() < 2 {
            continue;
        }
        for &owner in owners {
            if let Some(base) = rows[owner].base.as_deref() {
                blocked_bases.insert(class_key(base));
            }
            rows[owner].metadata_issues.push("proxy_identity_reused");
        }
    }

    let mut implementation_owners = HashMap::<i64, Vec<(usize, String)>>::new();
    let mut implementation_is_base = vec![false; rows.len()];
    for (pending_index, pending) in rows.iter_mut().enumerate() {
        let (Some(implementation_key), Some(base_key)) =
            (pending.implementation.as_deref(), pending.base.as_deref())
        else {
            continue;
        };
        let cache_key = (
            class_key(&pending.extension.to_class),
            class_key(implementation_key),
            class_key(&pending.expected_proxy),
        );
        if !implementation_cache.contains_key(&cache_key) {
            let lookup = implementation_lookup(
                conn,
                hierarchy,
                &pending.extension.to_class,
                Some(implementation_key),
                &pending.expected_proxy,
                &mut source_candidate_count,
            )?;
            implementation_cache.insert(cache_key.clone(), lookup);
        }
        let lookup = implementation_cache
            .get(&cache_key)
            .context("missing XenForo implementation lookup")?;
        let base_lookup_key = class_key(base_key);
        if !base_cache.contains_key(&base_lookup_key) {
            let lookup = base_lookup(hierarchy, base_key, &mut source_candidate_count)?;
            base_cache.insert(base_lookup_key.clone(), lookup);
        }
        let (base_classes, base_truncated) = base_cache
            .get(&base_lookup_key)
            .context("missing XenForo base lookup")?;
        if base_classes.len() > 1 || *base_truncated {
            blocked_bases.insert(base_lookup_key.clone());
            pending
                .metadata_issues
                .push("base_class_identity_ambiguous");
        }
        let base_ids = base_classes
            .iter()
            .map(|class| class.definition_id)
            .collect::<HashSet<_>>();
        if lookup
            .classes
            .iter()
            .any(|class| base_ids.contains(&class.definition_id))
        {
            blocked_bases.insert(base_lookup_key);
            implementation_is_base[pending_index] = true;
        }
        if lookup.classes.len() == 1 && !lookup.truncated {
            let class = &lookup.classes[0];
            implementation_owners
                .entry(class.definition_id)
                .or_default()
                .push((pending_index, class_key(&pending.expected_proxy)));
        }
    }
    for (pending_index, is_base) in implementation_is_base.into_iter().enumerate() {
        if is_base {
            rows[pending_index]
                .metadata_issues
                .push("implementation_is_base_class");
        }
    }
    for owners in implementation_owners.values() {
        let distinct_proxies = owners
            .iter()
            .map(|(_, proxy)| proxy)
            .collect::<HashSet<_>>();
        if distinct_proxies.len() < 2 {
            continue;
        }
        for (owner, _) in owners {
            if let Some(base) = rows[*owner].base.as_deref() {
                blocked_bases.insert(class_key(base));
            }
            rows[*owner]
                .metadata_issues
                .push("implementation_proxy_identity_conflict");
        }
    }

    let mut registrations = Vec::<Registration<RegistrationEvidence>>::new();
    let mut by_base = BTreeMap::<String, Vec<RegistrationIndex>>::new();
    let mut class_occurrence_updates = HashSet::<(i64, usize, usize, &'static str)>::new();

    for pending in &rows {
        let implementation_key = pending.implementation.as_deref();
        let mut implementation_classes = Vec::new();
        let mut implementation_truncated = false;
        if implementation_key.is_some() || pending.implementation.is_none() {
            let cache_canonical = implementation_key.map(class_key).unwrap_or_default();
            let cache_key = (
                class_key(&pending.extension.to_class),
                cache_canonical,
                class_key(&pending.expected_proxy),
            );
            if !implementation_cache.contains_key(&cache_key) {
                let lookup = implementation_lookup(
                    conn,
                    hierarchy,
                    &pending.extension.to_class,
                    implementation_key,
                    &pending.expected_proxy,
                    &mut source_candidate_count,
                )?;
                implementation_cache.insert(cache_key.clone(), lookup);
            }
            let lookup = implementation_cache
                .get(&cache_key)
                .context("missing XenForo implementation lookup")?;
            implementation_classes = lookup.classes.clone();
            implementation_truncated = lookup.truncated;
            push_class_occurrence_update(
                &mut prepared,
                &mut class_occurrence_updates,
                pending,
                "xenforo_extension_implementation",
                &pending.extension.to_class,
                &implementation_classes
                    .iter()
                    .map(|class| class.definition_id)
                    .collect::<Vec<_>>(),
                implementation_truncated,
                implementation_classes.len(),
                budget,
            )?;
        }
        if let Some(base_key) = pending.base.as_deref() {
            let base_lookup_key = class_key(base_key);
            if !base_cache.contains_key(&base_lookup_key) {
                let lookup = base_lookup(hierarchy, base_key, &mut source_candidate_count)?;
                base_cache.insert(base_lookup_key.clone(), lookup);
            }
            let (base_classes, base_truncated) = base_cache
                .get(&base_lookup_key)
                .context("missing XenForo base lookup")?;
            push_class_occurrence_update(
                &mut prepared,
                &mut class_occurrence_updates,
                pending,
                "xenforo_extension_base",
                &pending.extension.from_class,
                &base_classes
                    .iter()
                    .map(|class| class.definition_id)
                    .collect::<Vec<_>>(),
                *base_truncated,
                base_classes.len(),
                budget,
            )?;
        }

        let base = pending.base.as_deref();
        let base_key = base.map(class_key);
        let base_blocked = base_key
            .as_ref()
            .is_some_and(|key| blocked_bases.contains(key));
        let metadata_blocked = !pending.metadata_issues.is_empty();
        if base.is_none() || metadata_blocked || base_blocked {
            let mut reasons = pending.metadata_issues.clone();
            if base.is_none() {
                reasons.push("base_lookup_unavailable");
            }
            if base_blocked && reasons.is_empty() {
                reasons.push("canonical_base_chain_blocked");
            }
            if implementation_key.is_none() {
                reasons.push("implementation_lookup_unavailable");
            }
            if implementation_classes.is_empty() {
                reasons.push("implementation_class_missing");
            }
            if implementation_truncated {
                reasons.push("implementation_candidates_truncated");
            }
            let mut local = PreparedChainProjection {
                parent_edges: Vec::new(),
                placeholder_updates: Vec::new(),
                class_occurrence_updates: Vec::new(),
                hierarchy_edges: Vec::new(),
                issues: Vec::new(),
            };
            for class in &implementation_classes {
                let mut class_reasons = reasons.clone();
                class_reasons.extend(proxy_readiness_issues(class, &pending.expected_proxy));
                add_incomplete_proxy_edge(
                    &mut local,
                    class,
                    pending,
                    join_reasons(&class_reasons),
                    Vec::new(),
                    None,
                )?;
            }
            if implementation_classes.is_empty()
                || implementation_classes
                    .iter()
                    .all(|class| !class.proxy_matches || class.proxy_occurrence.is_none())
            {
                push_issue(&mut local, None, pending, join_reasons(&reasons));
            }
            charge_and_append(&mut prepared, local, budget)?;
            continue;
        }

        let Some(priority) = pending.extension.execute_order else {
            continue;
        };
        if pending.extension.active != Some(true) || pending.extension.duplicate {
            continue;
        }
        let implementation_key =
            implementation_key.context("validated XenForo implementation key disappeared")?;
        let evidence = RegistrationEvidence {
            file_id: pending.file.file_id,
            row_index: pending.row_index,
            tag_span: pending.extension.tag_span.clone(),
            base_name: base.expect("checked above").to_owned(),
            implementation_name: pending.extension.to_class.clone(),
            implementation_raw_lookup: class_key(&pending.extension.to_class),
            implementation_lookup: class_key(implementation_key),
            expected_proxy: pending.expected_proxy.clone(),
            priority,
        };
        let registration_index = RegistrationIndex(registrations.len());
        registrations.push(Registration {
            id: format!(
                "{}:{}:{}",
                pending.file.file_id, pending.row_index, implementation_key
            ),
            base: base_key.expect("canonical base key exists"),
            implementation: class_key(implementation_key),
            numeric_priority: priority,
            evidence,
        });
        by_base
            .entry(registrations[registration_index.0].base.clone())
            .or_default()
            .push(registration_index);
    }

    for (base, indices) in by_base {
        let graph = build_chain_for_base(&base, &indices, &registrations);
        let local = project_base_graph(
            &graph,
            &indices,
            &registrations,
            &implementation_cache,
            &base_cache,
        )?;
        charge_and_append(&mut prepared, local, budget)?;
    }

    coalesce_placeholder_updates(&mut prepared)?;
    Ok(prepared)
}

fn implementation_lookup(
    conn: &Connection,
    hierarchy: &PhpHierarchyIndex,
    raw_implementation: &str,
    canonical_implementation: Option<&str>,
    expected_proxy: &str,
    source_candidate_count: &mut usize,
) -> Result<ImplementationLookup> {
    let mut candidates = hierarchy
        .raw_class_candidates(raw_implementation)
        .take(MAX_CANDIDATES + 1)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        if let Some(canonical_implementation) = canonical_implementation {
            candidates = hierarchy
                .canonical_class_candidates(canonical_implementation)
                .take(MAX_CANDIDATES + 1)
                .collect::<Vec<_>>();
        }
    }
    let truncated = candidates.len() > MAX_CANDIDATES;
    candidates.truncate(MAX_CANDIDATES);
    charge_source_candidates(source_candidate_count, candidates.len())?;

    let expected_key = class_key(expected_proxy);
    let mut classes = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let parent_facts = hierarchy
            .parent_candidates(candidate.definition_id)
            .iter()
            .filter(|parent| parent.kind == PhpParentKind::XfcpProxy)
            .collect::<Vec<_>>();
        let proxy_parent = (parent_facts.len() == 1).then(|| parent_facts[0].clone());
        let proxy_matches = proxy_parent.as_ref().is_some_and(|parent| {
            parent
                .resolved_name
                .as_deref()
                .is_some_and(|name| class_key(name) == expected_key)
        });
        let proxy_occurrence = if proxy_matches {
            let parent = proxy_parent
                .as_ref()
                .context("validated XFCP parent disappeared")?;
            let start = usize::try_from(parent.evidence_start)?;
            let end = usize::try_from(parent.evidence_end)?;
            find_type_occurrence(conn, candidate.file_id, start..end)?
                .map(|occurrence_id| (occurrence_id, start..end))
        } else {
            None
        };
        classes.push(ImplementationClass {
            definition_id: candidate.definition_id,
            file_id: candidate.file_id,
            kind: candidate.kind,
            role: candidate.role,
            complete: candidate.complete,
            conditional: candidate.conditional,
            proxy_parent,
            proxy_occurrence,
            proxy_matches,
        });
    }
    Ok(ImplementationLookup { classes, truncated })
}

fn base_lookup(
    hierarchy: &PhpHierarchyIndex,
    canonical_base: &str,
    source_candidate_count: &mut usize,
) -> Result<(Vec<ClassTarget>, bool)> {
    let mut classes = hierarchy
        .class_candidates(canonical_base)
        .take(MAX_CANDIDATES + 1)
        .map(|candidate| ClassTarget {
            definition_id: candidate.definition_id,
            kind: candidate.kind,
            role: candidate.role,
            complete: candidate.complete,
            conditional: candidate.conditional,
        })
        .collect::<Vec<_>>();
    let truncated = classes.len() > MAX_CANDIDATES;
    classes.truncate(MAX_CANDIDATES);
    charge_source_candidates(source_candidate_count, classes.len())?;
    Ok((classes, truncated))
}

fn find_type_occurrence(
    conn: &Connection,
    file_id: i64,
    span: Range<usize>,
) -> Result<Option<i64>> {
    let mut statement = conn.prepare(
        "SELECT id,provenance FROM occurrences WHERE file_id=?1 AND start=?2 AND end=?3 AND role='type' ORDER BY id",
    )?;
    let mut rows = statement.query(params![
        file_id,
        i64::try_from(span.start)?,
        i64::try_from(span.end)?
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let occurrence_id: i64 = row.get(0)?;
    let provenance: String = row.get(1)?;
    ensure!(
        rows.next()?.is_none(),
        "XFCP parent token matches multiple PHP type occurrences"
    );
    Ok(
        (provenance == "xenforo_generated_placeholder" || provenance.starts_with("php_fqcn"))
            .then_some(occurrence_id),
    )
}

fn push_class_occurrence_update(
    prepared: &mut PreparedChainProjection,
    seen: &mut HashSet<(i64, usize, usize, &'static str)>,
    pending: &PendingRow<'_>,
    role: &'static str,
    name: &str,
    class_ids: &[i64],
    truncated: bool,
    total_candidates: usize,
    budget: &mut DerivedBudget,
) -> Result<()> {
    let span = if role == "xenforo_extension_base" {
        pending.extension.from_span.clone()
    } else {
        pending.extension.to_span.clone()
    };
    let identity = (pending.file.file_id, span.start, span.end, role);
    if !seen.insert(identity) {
        bail!("duplicate XenForo class metadata occurrence update");
    }
    let mut candidates = class_ids.to_vec();
    candidates.sort_unstable();
    candidates.dedup();
    if candidates.len() > MAX_CANDIDATES {
        candidates.truncate(MAX_CANDIDATES);
    }
    let was_truncated = truncated || total_candidates > MAX_CANDIDATES;
    budget.charge(candidates.len().saturating_add(candidates.len().max(1)))?;
    let mut provenance = format!(
        "xenforo_class_extensions_xml;{};runtime_order_unverified",
        issue_provenance(pending, "metadata_candidate")
    );
    if was_truncated {
        provenance.push_str(";xenforo_candidates_truncated");
    }
    prepared
        .class_occurrence_updates
        .push(PreparedClassOccurrenceUpdate {
            file_id: pending.file.file_id,
            span,
            role,
            name: name.to_owned(),
            class_ids: candidates,
            tag_span: pending.extension.tag_span.clone(),
            provenance,
        });
    Ok(())
}

fn charge_source_candidates(total: &mut usize, count: usize) -> Result<()> {
    *total = total.saturating_add(count);
    ensure!(
        *total <= MAX_METADATA_FACTS,
        "XenForo class candidate fact limit exceeded"
    );
    Ok(())
}
