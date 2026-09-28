use super::super::hierarchy_index::{
    PhpClassFact, PhpClassKind, PhpClassRole, PhpHierarchyIndex, PhpMethodFact, PhpParentCandidate,
    PhpParentKind, PhpParentStatus, PhpVisibility,
};
use super::{ParentLookup, ParentLookupIssueKind, lookup_parent_method};

fn class_fact(id: i64, name: &str) -> PhpClassFact {
    let fqcn = format!("Fixture\\{name}");
    PhpClassFact {
        definition_id: id,
        file_id: id,
        file_path: format!("src/{name}.php"),
        canonical_fqcn: Some(fqcn.to_ascii_lowercase()),
        fqcn,
        kind: PhpClassKind::Class,
        role: PhpClassRole::Ordinary,
        complete: true,
        conditional: false,
        has_trait_use: false,
        trait_use_conditional: false,
    }
}

fn method_fact(id: i64, owner_class_id: i64, name: &str) -> PhpMethodFact {
    PhpMethodFact {
        definition_id: id,
        owner_class_id,
        file_id: owner_class_id,
        file_path: format!("src/Class{owner_class_id}.php"),
        name: name.to_owned(),
        visibility: PhpVisibility::Public,
        abstract_method: false,
        complete: true,
        conditional: false,
    }
}

fn parent_fact(child_class_id: i64, candidate_class_ids: &[i64]) -> PhpParentCandidate {
    PhpParentCandidate {
        child_class_id,
        declared_name: "Fixture\\Parent".to_owned(),
        resolved_name: Some("Fixture\\Parent".to_owned()),
        canonical_name: Some("fixture\\parent".to_owned()),
        evidence_start: 10,
        evidence_end: 20,
        kind: PhpParentKind::Extends,
        candidate_class_ids: candidate_class_ids.to_vec(),
        status: if candidate_class_ids.is_empty() {
            PhpParentStatus::Missing
        } else {
            PhpParentStatus::Candidates
        },
        conditional: false,
        execute_order: None,
        tie_group: None,
    }
}

fn parent_fact_with_status(
    child_class_id: i64,
    candidate_class_ids: &[i64],
    status: PhpParentStatus,
    conditional: bool,
) -> PhpParentCandidate {
    let mut fact = parent_fact(child_class_id, candidate_class_ids);
    fact.status = status;
    fact.conditional = conditional;
    fact
}

fn index(
    classes: &[PhpClassFact],
    methods: &[PhpMethodFact],
    parents: &[PhpParentCandidate],
) -> PhpHierarchyIndex {
    PhpHierarchyIndex::from_facts(classes.to_vec(), methods.to_vec(), parents.to_vec())
}

fn candidates(result: &ParentLookup) -> Vec<i64> {
    let mut ids = result.candidates.clone();
    ids.sort_unstable();
    ids
}

fn has_issue(result: &ParentLookup, expected: ParentLookupIssueKind) -> bool {
    result.issues.iter().any(|issue| issue.kind == expected)
}

#[test]
fn nearest_override_is_selected_independently_on_each_parent_branch() {
    let hierarchy = index(
        &[
            class_fact(1, "Caller"),
            class_fact(2, "BranchA"),
            class_fact(3, "BranchB"),
            class_fact(4, "BranchBAncestor"),
        ],
        &[method_fact(102, 2, "save"), method_fact(104, 4, "save")],
        &[parent_fact(1, &[2, 3]), parent_fact(3, &[4])],
    );

    let result = lookup_parent_method(&hierarchy, 1, "save");

    assert_eq!(candidates(&result), [102, 104]);
}

#[test]
fn method_lookup_skips_framework_layer_without_method() {
    let hierarchy = index(
        &[
            class_fact(1, "Extension"),
            class_fact(2, "FrameworkLayer"),
            class_fact(3, "ApplicationAncestor"),
        ],
        &[method_fact(303, 3, "render")],
        &[parent_fact(1, &[2]), parent_fact(2, &[3])],
    );

    let result = lookup_parent_method(&hierarchy, 1, "render");

    assert_eq!(candidates(&result), [303]);
}

#[test]
fn strict_parent_chain_stops_at_the_nearest_override() {
    let hierarchy = index(
        &[
            class_fact(1, "Caller"),
            class_fact(2, "Middle"),
            class_fact(3, "Base"),
        ],
        &[method_fact(202, 2, "save"), method_fact(203, 3, "save")],
        &[parent_fact(1, &[2]), parent_fact(2, &[3])],
    );

    let result = lookup_parent_method(&hierarchy, 1, "save");

    assert_eq!(candidates(&result), [202]);
}

#[test]
fn tied_predecessors_are_kept_and_caller_is_excluded() {
    let mut tied = parent_fact(1, &[1, 2, 3]);
    tied.execute_order = Some(10);
    tied.tie_group = Some(7);
    let hierarchy = index(
        &[
            class_fact(1, "Caller"),
            class_fact(2, "TiedA"),
            class_fact(3, "TiedB"),
        ],
        &[
            method_fact(101, 1, "dispatch"),
            method_fact(202, 2, "dispatch"),
            method_fact(303, 3, "dispatch"),
        ],
        &[tied],
    );

    let result = lookup_parent_method(&hierarchy, 1, "dispatch");

    assert_eq!(candidates(&result), [202, 303]);
    assert!(!has_issue(
        &result,
        ParentLookupIssueKind::OrdinaryParentCycle
    ));
}

#[test]
fn missing_ambiguous_and_conditional_parents_block_the_branch() {
    let cases = [
        (
            PhpParentStatus::Missing,
            &[][..],
            false,
            ParentLookupIssueKind::MissingParent,
        ),
        (
            PhpParentStatus::Ambiguous,
            &[2, 3][..],
            false,
            ParentLookupIssueKind::AmbiguousParent,
        ),
        (
            PhpParentStatus::Conditional,
            &[2][..],
            true,
            ParentLookupIssueKind::ConditionalParent,
        ),
    ];

    for (status, parent_ids, conditional, expected_issue) in cases {
        let mut parents = vec![parent_fact_with_status(1, parent_ids, status, conditional)];
        parents.push(parent_fact(2, &[4]));
        parents.push(parent_fact(3, &[4]));
        let hierarchy = index(
            &[
                class_fact(1, "Caller"),
                class_fact(2, "PossibleParentA"),
                class_fact(3, "PossibleParentB"),
                class_fact(4, "Ancestor"),
            ],
            &[method_fact(404, 4, "render")],
            &parents,
        );

        let result = lookup_parent_method(&hierarchy, 1, "render");

        assert!(candidates(&result).is_empty());
        assert!(has_issue(&result, expected_issue));
    }
}

#[test]
fn private_abstract_and_conditional_methods_block_ancestor_fallback() {
    let cases = [
        (
            PhpVisibility::Private,
            false,
            false,
            ParentLookupIssueKind::PrivateMethod,
        ),
        (
            PhpVisibility::Public,
            true,
            false,
            ParentLookupIssueKind::AbstractMethod,
        ),
        (
            PhpVisibility::Public,
            false,
            true,
            ParentLookupIssueKind::ConditionalMethod,
        ),
    ];

    for (visibility, abstract_method, conditional, expected_issue) in cases {
        let mut blocker = method_fact(202, 2, "render");
        blocker.visibility = visibility;
        blocker.abstract_method = abstract_method;
        blocker.conditional = conditional;
        let hierarchy = index(
            &[
                class_fact(1, "Caller"),
                class_fact(2, "BlockingParent"),
                class_fact(3, "Ancestor"),
            ],
            &[blocker, method_fact(303, 3, "render")],
            &[parent_fact(1, &[2]), parent_fact(2, &[3])],
        );

        let result = lookup_parent_method(&hierarchy, 1, "render");

        assert!(candidates(&result).is_empty());
        assert!(has_issue(&result, expected_issue));
    }
}

#[test]
fn ambiguous_method_declarations_block_ancestor_fallback() {
    let hierarchy = index(
        &[
            class_fact(1, "Caller"),
            class_fact(2, "AmbiguousParent"),
            class_fact(3, "Ancestor"),
        ],
        &[
            method_fact(202, 2, "render"),
            method_fact(203, 2, "render"),
            method_fact(303, 3, "render"),
        ],
        &[parent_fact(1, &[2]), parent_fact(2, &[3])],
    );

    let result = lookup_parent_method(&hierarchy, 1, "render");

    assert!(candidates(&result).is_empty());
    assert!(has_issue(&result, ParentLookupIssueKind::AmbiguousMethod));
}

#[test]
fn trait_use_blocks_only_when_the_class_has_no_own_method() {
    let mut trait_user = class_fact(2, "TraitUser");
    trait_user.has_trait_use = true;
    let hierarchy_with_method = index(
        &[
            class_fact(1, "Caller"),
            trait_user.clone(),
            class_fact(3, "Ancestor"),
        ],
        &[method_fact(202, 2, "render"), method_fact(303, 3, "render")],
        &[parent_fact(1, &[2]), parent_fact(2, &[3])],
    );
    let own_method = lookup_parent_method(&hierarchy_with_method, 1, "render");
    assert_eq!(candidates(&own_method), [202]);

    let hierarchy_without_method = index(
        &[
            class_fact(1, "Caller"),
            trait_user,
            class_fact(3, "Ancestor"),
        ],
        &[method_fact(303, 3, "render")],
        &[parent_fact(1, &[2]), parent_fact(2, &[3])],
    );
    let trait_barrier = lookup_parent_method(&hierarchy_without_method, 1, "render");
    assert!(candidates(&trait_barrier).is_empty());
    assert!(has_issue(&trait_barrier, ParentLookupIssueKind::TraitUse));
}

#[test]
fn actual_parent_cycle_terminates_without_returning_the_caller_method() {
    let hierarchy = index(
        &[class_fact(1, "Caller"), class_fact(2, "Loop")],
        &[method_fact(101, 1, "render")],
        &[parent_fact(1, &[2]), parent_fact(2, &[1])],
    );

    let result = lookup_parent_method(&hierarchy, 1, "render");

    assert!(candidates(&result).is_empty());
    assert!(!result.truncated);
    assert!(has_issue(
        &result,
        ParentLookupIssueKind::OrdinaryParentCycle
    ));
}

#[test]
fn traversal_caps_mark_candidate_and_visited_results_incomplete() {
    let wide_hierarchy = index(
        &[class_fact(1, "Caller"), class_fact(2, "RepeatedCandidate")],
        &[method_fact(1002, 2, "render")],
        &[parent_fact(1, &[2; 65])],
    );
    let wide_result = lookup_parent_method(&wide_hierarchy, 1, "render");
    assert_eq!(candidates(&wide_result), [1002]);
    assert!(wide_result.truncated);
    assert!(has_issue(
        &wide_result,
        ParentLookupIssueKind::CandidateLimit
    ));
    assert!(!has_issue(&wide_result, ParentLookupIssueKind::VisitLimit));

    let chain_len = 80;
    let mut classes = (1..=chain_len)
        .map(|id| class_fact(id, &format!("Chain{id}")))
        .collect::<Vec<_>>();
    classes.shrink_to_fit();
    let parents = (1..chain_len)
        .map(|id| parent_fact(id, &[id + 1]))
        .collect::<Vec<_>>();
    let deep_hierarchy = index(&classes, &[], &parents);
    let deep_result = lookup_parent_method(&deep_hierarchy, 1, "render");
    assert!(deep_result.candidates.is_empty());
    assert!(deep_result.truncated);
    assert!(has_issue(&deep_result, ParentLookupIssueKind::VisitLimit));
}

#[test]
fn method_lookup_ignores_ascii_case() {
    let hierarchy = index(
        &[class_fact(1, "Caller"), class_fact(2, "Parent")],
        &[method_fact(202, 2, "RebuildCombination")],
        &[parent_fact(1, &[2])],
    );

    let result = lookup_parent_method(&hierarchy, 1, "rebuildcombination");

    assert_eq!(candidates(&result), [202]);
}
