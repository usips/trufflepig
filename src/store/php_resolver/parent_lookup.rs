use super::hierarchy_index::{
    PhpClassFact, PhpHierarchyIndex, PhpMethodFact, PhpParentKind, PhpParentStatus, PhpVisibility,
};
use std::collections::{HashSet, VecDeque};

#[cfg(test)]
mod tests;

/// Maximum class facts examined, including the caller's seeded class.
pub(super) const MAX_PARENT_LOOKUP_VISITED: usize = 64;
/// Maximum parent alternatives considered for one class.
pub(super) const MAX_PARENT_LOOKUP_CANDIDATES: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ParentLookup {
    pub(super) candidates: Vec<i64>,
    pub(super) issues: Vec<ParentLookupIssue>,
    pub(super) truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ParentLookupIssue {
    pub(super) class_id: Option<i64>,
    pub(super) kind: ParentLookupIssueKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum ParentLookupIssueKind {
    MissingParent,
    AmbiguousParent,
    ConditionalParent,
    IncompleteParent,
    UnsupportedParent,
    IncompleteClass,
    ConditionalClass,
    TraitUse,
    ConditionalTraitUse,
    AmbiguousMethod,
    PrivateMethod,
    AbstractMethod,
    ConditionalMethod,
    IncompleteMethod,
    UnknownMethodVisibility,
    MissingMethod,
    OrdinaryParentCycle,
    VisitLimit,
    CandidateLimit,
}

impl ParentLookupIssueKind {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::MissingParent => "missing_parent",
            Self::AmbiguousParent => "ambiguous_parent",
            Self::ConditionalParent => "conditional_parent",
            Self::IncompleteParent => "incomplete_parent",
            Self::UnsupportedParent => "unsupported_parent",
            Self::IncompleteClass => "incomplete_class",
            Self::ConditionalClass => "conditional_class",
            Self::TraitUse => "trait_use",
            Self::ConditionalTraitUse => "conditional_trait_use",
            Self::AmbiguousMethod => "ambiguous_method",
            Self::PrivateMethod => "private_method",
            Self::AbstractMethod => "abstract_method",
            Self::ConditionalMethod => "conditional_method",
            Self::IncompleteMethod => "incomplete_method",
            Self::UnknownMethodVisibility => "unknown_method_visibility",
            Self::MissingMethod => "missing_method",
            Self::OrdinaryParentCycle => "ordinary_parent_cycle",
            Self::VisitLimit => "visit_limit",
            Self::CandidateLimit => "candidate_limit",
        }
    }
}

struct PendingClass {
    class_id: i64,
    path: Vec<i64>,
}

/// Finds the nearest safe method declaration along each candidate parent branch.
///
/// Results remain candidates. An uncertain declaration or edge ends only its branch.
pub(super) fn lookup_parent_method(
    index: &PhpHierarchyIndex,
    caller_class_id: i64,
    method_name: &str,
) -> ParentLookup {
    let mut result = ParentLookup {
        candidates: Vec::with_capacity(MAX_PARENT_LOOKUP_CANDIDATES),
        issues: Vec::with_capacity(MAX_PARENT_LOOKUP_CANDIDATES),
        truncated: false,
    };
    let Some(caller) = index.class_fact(caller_class_id) else {
        add_issue(
            &mut result,
            Some(caller_class_id),
            ParentLookupIssueKind::IncompleteClass,
        );
        return result;
    };
    if let Some(kind) = class_issue(caller) {
        add_issue(&mut result, Some(caller_class_id), kind);
        return result;
    }

    let mut visited = HashSet::with_capacity(MAX_PARENT_LOOKUP_VISITED);
    visited.insert(caller_class_id);
    let mut pending = VecDeque::with_capacity(MAX_PARENT_LOOKUP_VISITED);
    let mut visited_count = 1_usize;
    enqueue_parents(
        index,
        caller_class_id,
        &[caller_class_id],
        true,
        &mut pending,
        &mut visited,
        &mut visited_count,
        &mut result,
    );

    while let Some(current) = pending.pop_front() {
        let Some(class) = index.class_fact(current.class_id) else {
            add_issue(
                &mut result,
                Some(current.class_id),
                ParentLookupIssueKind::IncompleteClass,
            );
            continue;
        };
        if let Some(kind) = class_issue(class) {
            add_issue(&mut result, Some(current.class_id), kind);
            continue;
        }

        let methods = index
            .direct_methods(current.class_id, method_name)
            .take(2)
            .collect::<Vec<_>>();
        if !methods.is_empty() {
            if methods.len() != 1 {
                add_issue(
                    &mut result,
                    Some(current.class_id),
                    ParentLookupIssueKind::AmbiguousMethod,
                );
                continue;
            }
            record_method(&mut result, current.class_id, methods[0]);
            continue;
        }

        if class.trait_use_conditional {
            add_issue(
                &mut result,
                Some(current.class_id),
                ParentLookupIssueKind::ConditionalTraitUse,
            );
            continue;
        }
        if class.has_trait_use {
            add_issue(
                &mut result,
                Some(current.class_id),
                ParentLookupIssueKind::TraitUse,
            );
            continue;
        }

        enqueue_parents(
            index,
            current.class_id,
            &current.path,
            false,
            &mut pending,
            &mut visited,
            &mut visited_count,
            &mut result,
        );
    }

    result.candidates.sort_unstable();
    result.candidates.dedup();
    result
        .issues
        .sort_by_key(|issue| (issue.class_id, issue.kind));
    result.issues.dedup();
    result
}

#[allow(clippy::too_many_arguments)]
fn enqueue_parents(
    index: &PhpHierarchyIndex,
    class_id: i64,
    path: &[i64],
    caller: bool,
    pending: &mut VecDeque<PendingClass>,
    visited: &mut HashSet<i64>,
    visited_count: &mut usize,
    result: &mut ParentLookup,
) {
    let parents = index.parent_candidates(class_id);
    if parents.is_empty() {
        add_issue(
            result,
            Some(class_id),
            if caller {
                ParentLookupIssueKind::MissingParent
            } else {
                ParentLookupIssueKind::MissingMethod
            },
        );
        return;
    }

    let mut candidate_count = 0_usize;
    let mut expanded_edges = 0_usize;
    for parent in parents {
        if expanded_edges == MAX_PARENT_LOOKUP_CANDIDATES {
            result.truncated = true;
            add_issue(
                result,
                Some(class_id),
                ParentLookupIssueKind::CandidateLimit,
            );
            break;
        }
        expanded_edges += 1;
        if parent.conditional {
            parent_issue(result, class_id, ParentLookupIssueKind::ConditionalParent);
            continue;
        }
        match parent.status {
            PhpParentStatus::Missing => {
                parent_issue(result, class_id, ParentLookupIssueKind::MissingParent);
                continue;
            }
            PhpParentStatus::Ambiguous => {
                parent_issue(result, class_id, ParentLookupIssueKind::AmbiguousParent);
                continue;
            }
            PhpParentStatus::Conditional => {
                parent_issue(result, class_id, ParentLookupIssueKind::ConditionalParent);
                continue;
            }
            PhpParentStatus::Incomplete => {
                parent_issue(result, class_id, ParentLookupIssueKind::IncompleteParent);
                continue;
            }
            PhpParentStatus::Unsupported => {
                parent_issue(result, class_id, ParentLookupIssueKind::UnsupportedParent);
                continue;
            }
            PhpParentStatus::Candidates => {}
        }
        if parent.kind == PhpParentKind::Unknown {
            parent_issue(result, class_id, ParentLookupIssueKind::UnsupportedParent);
            continue;
        }

        if parent.candidate_class_ids.is_empty() {
            parent_issue(result, class_id, ParentLookupIssueKind::MissingParent);
            continue;
        }
        for &candidate_id in &parent.candidate_class_ids {
            if candidate_count == MAX_PARENT_LOOKUP_CANDIDATES {
                result.truncated = true;
                parent_issue(result, class_id, ParentLookupIssueKind::CandidateLimit);
                return;
            }
            candidate_count += 1;
            if path.contains(&candidate_id) {
                if parent.kind == PhpParentKind::Extends && parent.tie_group.is_none() {
                    parent_issue(result, class_id, ParentLookupIssueKind::OrdinaryParentCycle);
                }
                continue;
            }
            if visited.contains(&candidate_id) {
                continue;
            }
            if *visited_count == MAX_PARENT_LOOKUP_VISITED {
                result.truncated = true;
                parent_issue(result, class_id, ParentLookupIssueKind::VisitLimit);
                continue;
            }
            visited.insert(candidate_id);
            *visited_count += 1;
            let mut candidate_path = Vec::with_capacity(path.len().saturating_add(1));
            candidate_path.extend_from_slice(path);
            candidate_path.push(candidate_id);
            pending.push_back(PendingClass {
                class_id: candidate_id,
                path: candidate_path,
            });
        }
    }
}

fn parent_issue(result: &mut ParentLookup, class_id: i64, kind: ParentLookupIssueKind) {
    add_issue(result, Some(class_id), kind);
}

fn add_issue(result: &mut ParentLookup, class_id: Option<i64>, kind: ParentLookupIssueKind) {
    push_issue(result, ParentLookupIssue { class_id, kind });
}

fn class_issue(class: &PhpClassFact) -> Option<ParentLookupIssueKind> {
    if class.conditional {
        Some(ParentLookupIssueKind::ConditionalClass)
    } else if !class.complete {
        Some(ParentLookupIssueKind::IncompleteClass)
    } else {
        None
    }
}

fn record_method(result: &mut ParentLookup, class_id: i64, method: &PhpMethodFact) {
    let blocker = if !method.complete {
        Some(ParentLookupIssueKind::IncompleteMethod)
    } else if method.conditional {
        Some(ParentLookupIssueKind::ConditionalMethod)
    } else if method.visibility == PhpVisibility::Private {
        Some(ParentLookupIssueKind::PrivateMethod)
    } else if method.abstract_method {
        Some(ParentLookupIssueKind::AbstractMethod)
    } else if method.visibility == PhpVisibility::Unknown {
        Some(ParentLookupIssueKind::UnknownMethodVisibility)
    } else {
        None
    };
    if let Some(kind) = blocker {
        add_issue(result, Some(class_id), kind);
    } else if result.candidates.len() == MAX_PARENT_LOOKUP_CANDIDATES {
        result.truncated = true;
        add_issue(
            result,
            Some(class_id),
            ParentLookupIssueKind::CandidateLimit,
        );
    } else if matches!(
        method.visibility,
        PhpVisibility::Public | PhpVisibility::Protected
    ) {
        result.candidates.push(method.definition_id);
    } else {
        add_issue(
            result,
            Some(class_id),
            ParentLookupIssueKind::UnknownMethodVisibility,
        );
    }
}

fn push_issue(result: &mut ParentLookup, issue: ParentLookupIssue) {
    if result.issues.contains(&issue) {
        return;
    }
    if result.issues.len() == MAX_PARENT_LOOKUP_CANDIDATES {
        result.truncated = true;
        return;
    }
    result.issues.push(issue);
}
