use super::*;
use std::collections::{BTreeSet, HashSet};

fn registration(id: &str, base: &str, implementation: &str, priority: i64) -> Registration<String> {
    Registration {
        id: id.to_owned(),
        base: base.to_owned(),
        implementation: implementation.to_owned(),
        numeric_priority: priority,
        evidence: id.to_owned(),
    }
}

fn edge_set(
    registrations: &[Registration<String>],
    graph: &ChainGraph<'_, String>,
) -> BTreeSet<(String, String, String)> {
    graph
        .edges
        .iter()
        .map(|edge| {
            let child = registrations[edge.child.0].id.clone();
            let predecessor = match edge.predecessor {
                PredecessorCandidate::Registration(index) => {
                    format!("registration:{}", registrations[index.0].id)
                }
                PredecessorCandidate::Base => "base".to_owned(),
            };
            (edge.base.to_owned(), child, predecessor)
        })
        .collect()
}

fn predecessor_set(
    registrations: &[Registration<String>],
    graph: &ChainGraph<'_, String>,
    child_id: &str,
) -> BTreeSet<String> {
    graph
        .edges
        .iter()
        .filter(|edge| registrations[edge.child.0].id == child_id)
        .map(|edge| match edge.predecessor {
            PredecessorCandidate::Registration(index) => registrations[index.0].id.clone(),
            PredecessorCandidate::Base => "base".to_owned(),
        })
        .collect()
}

fn assert_predecessors(
    registrations: &[Registration<String>],
    graph: &ChainGraph<'_, String>,
    expected: &[(&str, &[&str])],
) {
    for (child, predecessors) in expected {
        assert_eq!(
            predecessor_set(registrations, graph, child),
            predecessors
                .iter()
                .map(|item| (*item).to_owned())
                .collect::<BTreeSet<_>>(),
            "unexpected predecessors for {child}"
        );
    }
}

fn tie_issues(
    registrations: &[Registration<String>],
    graph: &ChainGraph<'_, String>,
) -> BTreeSet<(String, i64, Vec<String>)> {
    graph
        .issues
        .iter()
        .filter_map(|issue| {
            let ChainIssue::PriorityTie {
                base,
                numeric_priority,
                registrations: indices,
            } = issue
            else {
                return None;
            };
            let mut ids = indices
                .iter()
                .map(|index| registrations[index.0].id.clone())
                .collect::<Vec<_>>();
            ids.sort();
            Some(((*base).to_owned(), *numeric_priority, ids))
        })
        .collect()
}

#[test]
fn strict_three_chain_uses_priority_when_source_order_is_inverted() {
    let registrations = vec![
        registration("last", "Core\\Base", "Addon\\Last", 30),
        registration("first", "Core\\Base", "Addon\\First", 10),
        registration("middle", "Core\\Base", "Addon\\Middle", 20),
    ];
    let graph = build_chains(&registrations, &HashSet::new());

    assert_predecessors(
        &registrations,
        &graph,
        &[
            ("first", &["base"]),
            ("middle", &["first"]),
            ("last", &["middle"]),
        ],
    );
    assert!(graph.issues.is_empty());

    let middle_edge = graph
        .edges
        .iter()
        .find(|edge| registrations[edge.child.0].id == "middle")
        .expect("middle must have an immediate predecessor");
    assert_eq!(middle_edge.evidence.child, "middle");
    assert_eq!(
        middle_edge.evidence.predecessor.map(String::as_str),
        Some("first")
    );
}

#[test]
fn numeric_priorities_sort_as_numbers() {
    let registrations = vec![
        registration("p100", "Core\\Numeric", "Addon\\Hundred", 100),
        registration("p10", "Core\\Numeric", "Addon\\Ten", 10),
        registration("p2", "Core\\Numeric", "Addon\\Two", 2),
    ];
    let graph = build_chains(&registrations, &HashSet::new());

    assert_predecessors(
        &registrations,
        &graph,
        &[("p2", &["base"]), ("p10", &["p2"]), ("p100", &["p10"])],
    );
}

#[test]
fn five_way_tie_keeps_every_peer_and_base_candidate() {
    let registrations = (1..=5)
        .map(|number| {
            registration(
                &format!("tie{number}"),
                "Core\\Tied",
                &format!("Addon\\Tie{number}"),
                10,
            )
        })
        .collect::<Vec<_>>();
    let graph = build_chains(&registrations, &HashSet::new());

    for child in 1..=5 {
        let mut expected = (1..=5)
            .filter(|peer| *peer != child)
            .map(|peer| format!("tie{peer}"))
            .collect::<BTreeSet<_>>();
        expected.insert("base".to_owned());
        assert_eq!(
            predecessor_set(&registrations, &graph, &format!("tie{child}")),
            expected
        );
    }
    assert_eq!(graph.edges.len(), 25);
    assert_eq!(
        tie_issues(&registrations, &graph),
        BTreeSet::from([(
            "Core\\Tied".to_owned(),
            10,
            (1..=5).map(|n| format!("tie{n}")).collect::<Vec<_>>(),
        )])
    );
}

#[test]
fn tied_groups_point_only_to_same_and_immediately_previous_priorities() {
    let registrations = vec![
        registration("e", "Core\\Groups", "Addon\\E", 30),
        registration("c", "Core\\Groups", "Addon\\C", 20),
        registration("a", "Core\\Groups", "Addon\\A", 10),
        registration("f", "Core\\Groups", "Addon\\F", 30),
        registration("d", "Core\\Groups", "Addon\\D", 20),
        registration("b", "Core\\Groups", "Addon\\B", 10),
    ];
    let graph = build_chains(&registrations, &HashSet::new());

    assert_predecessors(
        &registrations,
        &graph,
        &[
            ("a", &["base", "b"]),
            ("b", &["base", "a"]),
            ("c", &["a", "b", "d"]),
            ("d", &["a", "b", "c"]),
            ("e", &["c", "d", "f"]),
            ("f", &["c", "d", "e"]),
        ],
    );
    assert_eq!(
        tie_issues(&registrations, &graph),
        BTreeSet::from([
            ("Core\\Groups".to_owned(), 10, vec!["a".into(), "b".into()]),
            ("Core\\Groups".to_owned(), 20, vec!["c".into(), "d".into()]),
            ("Core\\Groups".to_owned(), 30, vec!["e".into(), "f".into()]),
        ])
    );
}

#[test]
fn missing_base_class_stays_an_explicit_boundary() {
    let missing_base = "Vendor\\AbsentCore";
    let registrations = vec![
        registration("later", missing_base, "Addon\\Later", 20),
        registration("first", missing_base, "Addon\\First", 10),
        registration("other", "Vendor\\OtherCore", "Addon\\Other", 10),
    ];
    let graph = build_chains(&registrations, &HashSet::new());

    assert_predecessors(
        &registrations,
        &graph,
        &[
            ("first", &["base"]),
            ("later", &["first"]),
            ("other", &["base"]),
        ],
    );
    assert_eq!(
        graph
            .edges
            .iter()
            .find(|edge| registrations[edge.child.0].id == "first")
            .map(|edge| edge.base),
        Some(missing_base)
    );
}

#[test]
fn input_permutation_preserves_the_normalized_candidate_graph() {
    let first_order = vec![
        registration("a", "Core\\Permutation", "Addon\\A", 10),
        registration("b", "Core\\Permutation", "Addon\\B", 10),
        registration("c", "Core\\Permutation", "Addon\\C", 20),
        registration("d", "Core\\OtherPermutation", "Addon\\D", 5),
    ];
    let second_order = vec![
        first_order[3].clone(),
        first_order[2].clone(),
        first_order[1].clone(),
        first_order[0].clone(),
    ];
    let first_graph = build_chains(&first_order, &HashSet::new());
    let second_graph = build_chains(&second_order, &HashSet::new());

    assert_eq!(
        edge_set(&first_order, &first_graph),
        edge_set(&second_order, &second_graph)
    );
    assert_eq!(
        tie_issues(&first_order, &first_graph),
        tie_issues(&second_order, &second_graph)
    );
}

#[test]
fn sixty_four_class_boundary_emits_the_complete_chain() {
    let registrations = (0..63)
        .map(|index| {
            registration(
                &format!("r{index}"),
                "Core\\AtLimit",
                &format!("Addon\\AtLimit{index}"),
                index,
            )
        })
        .collect::<Vec<_>>();
    let graph = build_chains(&registrations, &HashSet::new());

    assert_eq!(graph.edges.len(), 63);
    assert!(graph.issues.is_empty());
    assert_eq!(edge_set(&registrations, &graph).len(), registrations.len());
    assert_predecessors(
        &registrations,
        &graph,
        &[("r0", &["base"]), ("r62", &["r61"])],
    );
}

#[test]
fn sixty_five_classes_report_limit_without_partial_edges() {
    let registrations = (0..64)
        .map(|index| {
            registration(
                &format!("r{index}"),
                "Core\\OverLimit",
                &format!("Addon\\OverLimit{index}"),
                index,
            )
        })
        .collect::<Vec<_>>();
    let graph = build_chains(&registrations, &HashSet::new());

    assert!(graph.edges.is_empty());
    assert_eq!(graph.issues.len(), 1);
    match &graph.issues[0] {
        ChainIssue::ClassLimitExceeded {
            base,
            class_count,
            limit,
        } => {
            assert_eq!(*base, "Core\\OverLimit");
            assert_eq!(*class_count, 65);
            assert_eq!(*limit, MAX_CHAIN_CLASSES);
        }
        issue => panic!("expected class-limit issue, got {issue:?}"),
    }
}
