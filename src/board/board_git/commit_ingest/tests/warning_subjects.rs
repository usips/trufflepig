use super::super::commit_warnings::commit_warning_oid;
use super::*;

#[test]
fn subject_line_plan_is_not_misplaced() {
    let fixture = GitFixture::new();
    let subject = fixture.commit("Plan: P5");
    let report = RepoIngestor::default()
        .ingest(
            &mut TestBackend::default(),
            &actor(),
            &[target(&fixture)],
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(
        report.errors.is_empty(),
        "a Plan-shaped subject is not a misplaced trailer ({subject}): {:?}",
        report.errors
    );
}

#[test]
fn grep_budget_exhaustion_warns_instead_of_failing() {
    let fixture = GitFixture::new();
    let linked = fixture.commit(&linked_message("Successful main scan"));
    let registration = fixture.registration();
    let deadline = Instant::now() + Duration::from_secs(5);
    let tips = commit_scanner::collect_tips(&registration, deadline).unwrap();
    commit_scanner::expire_next_auxiliary_budget();
    let scan = commit_scanner::scan_log(&registration, &tips, 1_700_000_000, true, deadline);
    assert!(
        scan.is_ok(),
        "the successful main scan must survive an exhausted grep budget: {:?}",
        scan.err()
    );
    let scan = scan.unwrap();
    assert!(scan.complete);
    assert_eq!(scan.records[0].commit.oid, linked);
    assert!(
        scan.warnings
            .iter()
            .any(|warning| warning.contains("trailer_check_skipped")),
        "{:?}",
        scan.warnings
    );
}

#[test]
fn grep_is_case_insensitive_and_unlinked_warnings_remain_nonfatal() {
    let fixture = GitFixture::new();
    let first =
        fixture.commit("First\n\npLaN: p7\n\nCo-authored-by: Model claim <noreply@openai.com>");
    let second = fixture
        .commit("Second\n\nPLAN-TASK: p7.3\n\nCo-authored-by: Model claim <noreply@openai.com>");
    let scan = find_unlinked(
        &fixture.registration(),
        1_700_000_000,
        &HarnessLabel::parse("codex").unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(scan.scan_error.is_none(), "{:?}", scan.scan_error);
    assert_eq!(scan.warnings.len(), 2);
    assert_eq!(
        scan.warnings
            .iter()
            .filter_map(|warning| commit_warning_oid(warning))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first, second])
    );
    assert!(scan.warnings.iter().all(|warning| !warning.contains("; ")));
}

#[test]
fn grep_conservatively_skips_a_subject_match_even_with_a_body_match() {
    let fixture = GitFixture::new();
    fixture.commit("pLaN-TaSk: p5.1\n\nPlan: P7\n\nBody prose.");
    let report = RepoIngestor::default()
        .ingest(
            &mut TestBackend::default(),
            &actor(),
            &[target(&fixture)],
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}
