use super::*;

#[test]
fn malformed_coauthor_keeps_plan_link_and_warns() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    let linked = fixture.commit(
        "linked\n\nPlan: P7\nPlan-Task: P7.3\nCo-authored-by: broken\nCo-authored-by: Model claim <noreply@openai.com>",
    );
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
    assert_eq!(report.completed.len(), 1);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("malformed co-author")),
        "{:?}",
        report.errors
    );
    let commit = backend
        .linked
        .iter()
        .find(|commit| commit.oid == linked)
        .expect("malformed co-author must not drop the plan link");
    assert_eq!(commit.plans.len(), 1);
    assert_eq!(commit.plans[0].task_ordinal, Some(3));
    assert_eq!(commit.coauthors.len(), 1);
    assert_eq!(commit.coauthors[0].harness.as_str(), "codex");
    assert_eq!(commit.coauthors[0].email, "noreply@openai.com");
    assert_eq!(
        ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap()
            .skipped,
        1
    );
}

#[test]
fn coauthor_overflow_keeps_bounded_prefix_and_warns() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    let trailers = (0..70)
        .map(|ordinal| format!("Co-authored-by: Agent {ordinal} <agent{ordinal}@example.test>"))
        .collect::<Vec<_>>()
        .join("\n");
    let linked = fixture.commit(&format!("crowded\n\nPlan: P7\n{trailers}"));
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
    assert_eq!(report.completed.len(), 1);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("co-author") && error.contains('6')),
        "{:?}",
        report.errors
    );
    let commit = backend
        .linked
        .iter()
        .find(|commit| commit.oid == linked)
        .expect("co-author overflow must not drop the plan link");
    assert_eq!(commit.coauthors.len(), 64);
    assert_eq!(commit.coauthors[0].model, "Agent 0");
    assert_eq!(commit.coauthors[63].model, "Agent 63");
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
fn latin1_coauthor_trailer_keeps_the_record_and_plan_link() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    fixture.git(&["config", "i18n.commitEncoding", "latin1"]);
    let message = fixture.root.join("message.txt");
    std::fs::write(
        &message,
        b"latin1 coauthor\n\nPlan: P7\nCo-authored-by: Caf\xe9 <c@example.test>\n",
    )
    .unwrap();
    fixture.git(&[
        "commit",
        "--allow-empty",
        "--quiet",
        "-F",
        message.to_str().unwrap(),
    ]);
    let linked = GitOid::parse(fixture.git(&["rev-parse", "HEAD"]).trim()).unwrap();
    let target = target(&fixture);
    let mut backend = TestBackend::default();
    let report = RepoIngestor::default()
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    let commit = backend
        .linked
        .iter()
        .find(|commit| commit.oid == linked)
        .expect("a non-UTF-8 trailer must not drop the commit");
    assert_eq!(commit.plans.len(), 1);
    assert_eq!(commit.coauthors.len(), 1);
    assert_eq!(commit.coauthors[0].model, "Caf\u{fffd}");
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("not valid UTF-8")),
        "{:?}",
        report.errors
    );
}
