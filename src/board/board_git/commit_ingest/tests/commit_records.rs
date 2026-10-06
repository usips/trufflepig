use super::*;
use crate::board::board_protocol::CommitPlanLink;

#[test]
fn native_trailers_link_case_insensitively_and_exclude_body_mentions() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    let body_mention =
        fixture.commit("body mention\n\nPlan: P7\n\nThis is body prose, not a trailer.");
    std::fs::write(fixture.root.join("source.txt"), "one\ntwo\n").unwrap();
    fixture.git(&["add", "source.txt"]);
    let linked = fixture.commit(
        "real footer\n\nPLAN: P7\nPlan-Task: P7.3\nCo-Authored-By: Model Claim <noreply@OpenAI.com>",
    );
    let folded = fixture.commit("folded footer\n\nPlan:\n P7\nPlan-Task:\n P7.3");
    let spaced = fixture.commit("spaced footer\n\nPlan : P7");
    let target = target(&fixture);
    let mut backend = TestBackend::default();
    let report = RepoIngestor::default()
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0].contains("misplaced_trailers")
            && report.errors[0].contains(&body_mention.to_string()),
        "a trailer-shaped body line warns without linking: {:?}",
        report.errors
    );
    assert_eq!(
        backend
            .linked
            .iter()
            .map(|commit| commit.oid)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([linked, folded, spaced])
    );
    let linked = backend
        .linked
        .iter()
        .find(|commit| commit.oid == linked)
        .unwrap();
    assert_eq!(linked.plans[0].task_ordinal, Some(3));
    assert_eq!(linked.coauthors[0].harness.as_str(), "codex");
    assert_eq!(linked.files, 1);
    assert_eq!(linked.insertions, 2);
}

#[test]
fn malformed_commit_is_isolated_and_valid_metadata_is_bounded() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    fixture.commit("bad task\n\nPlan: P7\nPlan-Task: invalid");
    let subject = "é".repeat(900);
    fixture.git(&["config", "user.name", &"é".repeat(900)]);
    let good = fixture.commit(&format!(
        "{subject}\n\nPlan: P7\nPlan-Task: P7.3\nPlan-Task: P7.4"
    ));
    let target = target(&fixture);
    let mut ingestor = RepoIngestor::default();
    let mut backend = TestBackend::default();
    let first = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[target.clone()],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(first.completed.len(), 1);
    assert_eq!(first.errors.len(), 1);
    assert!(
        first.errors[0].contains("malformed Plan-Task"),
        "{:?}",
        first.errors
    );
    assert_eq!(backend.linked.len(), 2);
    let degraded = backend
        .linked
        .iter()
        .find(|commit| commit.oid != good)
        .unwrap();
    assert_eq!(
        degraded.plans,
        vec![CommitPlanLink {
            plan_id: PlanId::new(7).unwrap(),
            task_ordinal: None,
        }]
    );
    let commit = &backend.linked[0];
    assert_eq!(commit.oid, good);
    assert!(commit.subject.len() <= 1024);
    assert!(commit.author.len() <= 1024);
    assert_eq!(
        commit
            .plans
            .iter()
            .map(|link| link.task_ordinal)
            .collect::<Vec<_>>(),
        vec![Some(3), Some(4)]
    );
    BoardRequest::new(
        actor(),
        BoardOp::LinkCommits {
            commits: backend.linked.clone(),
        },
    )
    .validate()
    .unwrap();
    assert_eq!(
        ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap()
            .skipped,
        1
    );
}

#[test]
fn misplaced_plan_trailers_warn_through_scan_reports_without_linking() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    // The exact W5-H1 shape (commit 49a77c3): blank lines between trailers,
    // so Git's final-paragraph rule parses only the co-author.
    let misplaced = fixture.commit(concat!(
        "feat(board): delegate claims to coder sessions\n",
        "\n",
        "Steward orchestrators claim tasks on behalf of coder sessions via 'board claim P7.3 SCOPE --for HARNESS/SESSION'.\n",
        "\n",
        "Red evidence: the delegation tests failed to compile before the change.\n",
        "\n",
        "Plan: P7\n",
        "\n",
        "Plan-Task: P7.3\n",
        "\n",
        "Co-authored-by: Muse Spark <noreply@meta.com>"
    ));
    let target = target(&fixture);
    let mut ingestor = RepoIngestor::default();
    let mut backend = TestBackend::default();
    let report = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[target.clone()],
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(
        backend.linked.is_empty(),
        "misplaced trailers never create links: {:?}",
        backend.linked
    );
    assert_eq!(
        backend.scan_errors.last(),
        Some(&None),
        "warnings must not fail the scan: {:?}",
        backend.scan_errors
    );
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("misplaced_trailers")
                && error.contains(&misplaced.to_string())),
        "{:?}",
        report.errors
    );
    // A cached stamp re-emits the warning until the commit is repaired.
    let second = ingestor
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert_eq!(second.skipped, 1);
    assert!(
        second
            .errors
            .iter()
            .any(|error| error.contains("misplaced_trailers")),
        "{:?}",
        second.errors
    );
}

#[test]
fn misplaced_trailer_warning_names_each_oid_once() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    let first = fixture.commit(concat!(
        "split footer one\n\nBody prose.\n\nPlan: P7\n\nPlan-Task: P7.3\n\n",
        "Co-authored-by: Muse Spark <noreply@meta.com>"
    ));
    fixture.commit("plain middle");
    let second = fixture.commit(concat!(
        "split footer two\n\nMore prose.\n\nPlan: P7\n\n",
        "Co-authored-by: Muse Spark <noreply@meta.com>"
    ));
    let target = target(&fixture);
    let mut backend = TestBackend::default();
    let report = RepoIngestor::default()
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    let scan = find_unlinked(
        &fixture.registration(),
        1_700_000_000,
        &HarnessLabel::parse("muse").unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    let mut warnings = report.errors.clone();
    if let Some(error) = &scan.scan_error {
        warnings.extend(error.split("; ").map(str::to_owned));
    }
    warnings.sort();
    warnings.dedup();
    for oid in [first, second] {
        assert_eq!(
            warnings
                .iter()
                .filter(|warning| warning.contains(oid.as_str()))
                .count(),
            1,
            "each misplaced oid is named once: {warnings:?}"
        );
    }
    assert!(
        warnings
            .iter()
            .all(|warning| !warning.contains("Git record")),
        "warnings key by oid, not record index: {warnings:?}"
    );
}

#[test]
fn plan_task_trailer_without_plan_warns_through_scan_reports() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    let task_only = fixture
        .commit("task only\n\nPlan-Task: P7.3\nCo-authored-by: Model claim <noreply@openai.com>");
    let target = target(&fixture);
    let mut backend = TestBackend::default();
    let report = RepoIngestor::default()
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert!(
        backend.linked.is_empty(),
        "a plan task without a plan never links: {:?}",
        backend.linked
    );
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("plan_task_without_plan")
                && error.contains(&task_only.to_string())),
        "{:?}",
        report.errors
    );
}
