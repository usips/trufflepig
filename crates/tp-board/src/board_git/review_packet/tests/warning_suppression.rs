use super::*;

#[test]
fn unrelated_warning_survives_linked_suppression() {
    let mut source = evidence();
    let linked = commit("codex", 120);
    let other_oid = GitOid::parse(&"c".repeat(40)).unwrap();
    let warnings = vec![
        format!(
            "board_scan: statistics unavailable while inspecting {}",
            linked.oid
        ),
        format!(
            "board_scan: {other_oid}: malformed field referencing {}",
            linked.oid
        ),
        format!("board_scan: {}0: malformed field", linked.oid),
    ];
    source.commits.push(linked.clone());
    let mut input = warnings.clone();
    input.push(format!("board_scan: {}: misplaced_trailers", linked.oid));
    let mut retained = input.clone();
    let packet = assemble_review(&source, None, &[], Vec::new(), input);
    retained.sort();
    assert_eq!(
        packet.scan_errors, retained,
        "assembly retains diagnostics that have no repository context"
    );
}

#[test]
fn same_oid_in_another_repo_preserves_its_warning_and_unlinked_commit() {
    let mut source = evidence();
    let linked = commit("codex", 120);
    let mut other_repo = linked.clone();
    other_repo.repo_key = RepoKey::from_roots([GitOid::parse(&"d".repeat(40)).unwrap()]).unwrap();
    other_repo.plans.clear();
    let warning = format!("board_scan: {}: misplaced_trailers", other_repo.oid);
    source.commits.push(linked);
    let packet = assemble_review(
        &source,
        None,
        &[],
        vec![other_repo.clone()],
        vec![warning.clone()],
    );
    assert_eq!(packet.unlinked.len(), 1);
    assert_eq!(packet.unlinked[0].commit.repo_key, other_repo.repo_key);
    assert_eq!(packet.scan_errors, vec![warning]);
}
