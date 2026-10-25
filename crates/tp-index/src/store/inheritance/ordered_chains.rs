//! Builds candidate predecessor edges from ordered class registrations.
//!
//! Class keys are canonicalized by the caller. Equal-priority edges are alternatives,
//! not a selected runtime order; missing classes remain declared chain boundaries.

#[cfg(test)]
use std::collections::{BTreeMap, HashSet};

#[cfg(test)]
mod tests;

pub const MAX_CHAIN_CLASSES: usize = 64;
pub const MAX_CANDIDATES_PER_REGISTRATION: usize = 64;

/// One validated registration, with evidence retained for each projected edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration<E> {
    pub id: String,
    pub base: String,
    pub implementation: String,
    pub numeric_priority: i64,
    pub evidence: E,
}

/// Stable index into the input registration slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegistrationIndex(pub usize);

/// A possible immediate predecessor for one registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PredecessorCandidate {
    Registration(RegistrationIndex),
    Base,
}

/// Evidence for both declarations that support a candidate edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeEvidence<'a, E> {
    pub child: &'a E,
    pub predecessor: Option<&'a E>,
}

/// One declared possible immediate-predecessor relationship.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateEdge<'a, E> {
    pub base: &'a str,
    pub child: RegistrationIndex,
    pub predecessor: PredecessorCandidate,
    pub evidence: EdgeEvidence<'a, E>,
}

/// Why a candidate chain is ambiguous or incomplete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainIssue<'a> {
    /// Members at this priority have no declared order among themselves.
    PriorityTie {
        base: &'a str,
        numeric_priority: i64,
        registrations: Vec<RegistrationIndex>,
    },
    /// The chain exceeds the supported class count and has no emitted edges.
    ClassLimitExceeded {
        base: &'a str,
        class_count: usize,
        limit: usize,
    },
    /// One registration has more predecessor alternatives than the supported bound.
    CandidateLimitExceeded {
        base: &'a str,
        child: RegistrationIndex,
        candidate_count: usize,
        limit: usize,
    },
}

/// Candidate predecessor graph and its ambiguity or completeness issues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainGraph<'a, E> {
    pub edges: Vec<CandidateEdge<'a, E>>,
    pub issues: Vec<ChainIssue<'a>>,
}

/// Builds immediate-predecessor alternatives for every unblocked canonical base.
///
/// Priorities are sorted ascending, while registration input order is preserved within
/// a tie. The blocked set is for invalid, duplicate, or conflicting metadata groups;
/// missing classes are intentionally not filtered here.
#[cfg(test)]
pub fn build_chains<'a, E>(
    registrations: &'a [Registration<E>],
    blocked_bases: &HashSet<String>,
) -> ChainGraph<'a, E> {
    let mut by_base = BTreeMap::<&str, Vec<RegistrationIndex>>::new();
    for (index, registration) in registrations.iter().enumerate() {
        if !blocked_bases.contains(registration.base.as_str()) {
            by_base
                .entry(registration.base.as_str())
                .or_default()
                .push(RegistrationIndex(index));
        }
    }

    let mut graph = ChainGraph {
        edges: Vec::new(),
        issues: Vec::new(),
    };
    for (base, mut indices) in by_base {
        let group_graph = build_base_chain(base, &mut indices, registrations);
        graph.edges.extend(group_graph.edges);
        graph.issues.extend(group_graph.issues);
    }
    graph
}

/// Builds one base group's candidate graph without aggregating other bases.
pub fn build_chain_for_base<'a, E>(
    base: &'a str,
    indices: &[RegistrationIndex],
    registrations: &'a [Registration<E>],
) -> ChainGraph<'a, E> {
    let mut indices = indices.to_vec();
    build_base_chain(base, &mut indices, registrations)
}

fn build_base_chain<'a, E>(
    base: &'a str,
    indices: &mut Vec<RegistrationIndex>,
    registrations: &'a [Registration<E>],
) -> ChainGraph<'a, E> {
    // Stable sorting groups priorities without imposing an order within a tie.
    indices.sort_by_key(|index| registrations[index.0].numeric_priority);

    let mut graph = ChainGraph {
        edges: Vec::new(),
        issues: Vec::new(),
    };
    let class_count = indices.len().saturating_add(1); // Includes the base class.
    if class_count > MAX_CHAIN_CLASSES {
        graph.issues.push(ChainIssue::ClassLimitExceeded {
            base,
            class_count,
            limit: MAX_CHAIN_CLASSES,
        });
        return graph;
    }

    let groups = priority_groups(indices, registrations);
    let mut candidate_overflow = None;
    for (group_index, group) in groups.iter().enumerate() {
        let group_len = group.len();
        let previous_len = group_index
            .checked_sub(1)
            .map(|previous| groups[previous].len())
            .unwrap_or(0);
        let candidate_count =
            group_len.saturating_sub(1) + if previous_len == 0 { 1 } else { previous_len };
        if candidate_count > MAX_CANDIDATES_PER_REGISTRATION {
            candidate_overflow = Some((group.start, candidate_count));
            break;
        }
    }
    if let Some((group_start, candidate_count)) = candidate_overflow {
        graph.issues.push(ChainIssue::CandidateLimitExceeded {
            base,
            child: indices[group_start],
            candidate_count,
            limit: MAX_CANDIDATES_PER_REGISTRATION,
        });
        return graph;
    }

    let edge_count = groups
        .iter()
        .enumerate()
        .map(|(group_index, group)| {
            let group_len = group.len();
            let previous_len = group_index
                .checked_sub(1)
                .map(|previous| groups[previous].len())
                .unwrap_or(0);
            group_len
                * (group_len.saturating_sub(1) + if previous_len == 0 { 1 } else { previous_len })
        })
        .sum::<usize>();
    graph.edges.reserve(edge_count);

    for (group_index, group) in groups.iter().enumerate() {
        let priority = registrations[indices[group.start].0].numeric_priority;
        if group.len() > 1 {
            graph.issues.push(ChainIssue::PriorityTie {
                base,
                numeric_priority: priority,
                registrations: indices[group.clone()].to_vec(),
            });
        }

        let previous_group = group_index
            .checked_sub(1)
            .map(|previous| &indices[groups[previous].clone()]);
        for &child in &indices[group.clone()] {
            for &peer in &indices[group.clone()] {
                if child != peer {
                    graph.edges.push(edge(
                        base,
                        child,
                        PredecessorCandidate::Registration(peer),
                        registrations,
                        Some(peer),
                    ));
                }
            }
            if let Some(previous_group) = previous_group {
                for &predecessor in previous_group {
                    graph.edges.push(edge(
                        base,
                        child,
                        PredecessorCandidate::Registration(predecessor),
                        registrations,
                        Some(predecessor),
                    ));
                }
            } else {
                graph.edges.push(edge(
                    base,
                    child,
                    PredecessorCandidate::Base,
                    registrations,
                    None,
                ));
            }
        }
    }
    graph
}

fn priority_groups<E>(
    indices: &[RegistrationIndex],
    registrations: &[Registration<E>],
) -> Vec<std::ops::Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0;
    while start < indices.len() {
        let priority = registrations[indices[start].0].numeric_priority;
        let mut end = start + 1;
        while end < indices.len() && registrations[indices[end].0].numeric_priority == priority {
            end += 1;
        }
        groups.push(start..end);
        start = end;
    }
    groups
}

fn edge<'a, E>(
    base: &'a str,
    child: RegistrationIndex,
    predecessor: PredecessorCandidate,
    registrations: &'a [Registration<E>],
    predecessor_index: Option<RegistrationIndex>,
) -> CandidateEdge<'a, E> {
    CandidateEdge {
        base,
        child,
        predecessor,
        evidence: EdgeEvidence {
            child: &registrations[child.0].evidence,
            predecessor: predecessor_index.map(|index| &registrations[index.0].evidence),
        },
    }
}
