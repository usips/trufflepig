use super::*;

#[test]
fn failed_backend_link_keeps_same_tip_scan_retryable() {
    let fixture = GitFixture::new();
    fixture.commit(&linked_message("linked root"));
    let target = target(&fixture);
    let mut ingestor = RepoIngestor::default();
    let mut backend = TestBackend {
        fail_next_link: true,
        ..TestBackend::default()
    };
    let first = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[target.clone()],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(first.errors.len(), 1);
    assert!(backend.scan_errors[0].is_some());
    let second = ingestor
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert!(second.errors.is_empty());
    assert_eq!(backend.requests, 2);
    assert_eq!(second.inserted, 1);
    assert_eq!(backend.scan_errors.last(), Some(&None));
}

#[test]
fn unknown_plan_advances_stamp_until_plan_set_changes() {
    let fixture = GitFixture::new();
    fixture.commit("future plan\n\nPlan: P99");
    let mut target = target(&fixture);
    let unknown = PlanId::new(99).unwrap();
    let mut backend = TestBackend {
        unknown_plans: vec![unknown],
        ..TestBackend::default()
    };
    let mut ingestor = RepoIngestor::default();
    let first = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[target.clone()],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(first.unknown_plans, vec![unknown]);
    let second = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[target.clone()],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(second.skipped, 1);
    assert_eq!(backend.requests, 1);
    assert_eq!(second.unknown_plans, vec![unknown]);
    backend.unknown_plans.clear();
    target.plans.push(unknown);
    let third = ingestor
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert_eq!(third.scanned, 1);
    assert_eq!(backend.requests, 2);
}

#[test]
fn missing_blob_statistics_do_not_block_commit_links_or_stamp() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    std::fs::write(
        fixture.root.join("unavailable.txt"),
        "missing object contents\n",
    )
    .unwrap();
    fixture.git(&["add", "unavailable.txt"]);
    let blob = fixture.git(&["rev-parse", ":unavailable.txt"]);
    let blob = blob.trim();
    let linked = fixture.commit(&linked_message("missing blob"));
    std::fs::remove_file(
        fixture
            .root
            .join(".git/objects")
            .join(&blob[..2])
            .join(&blob[2..]),
    )
    .unwrap();
    let target = target(&fixture);
    let mut backend = TestBackend::default();
    let mut ingestor = RepoIngestor::default();
    let first = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[target.clone()],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(first.completed.len(), 1);
    assert_eq!(backend.linked.len(), 1);
    assert_eq!(backend.linked[0].oid, linked);
    assert_eq!(backend.linked[0].files, 0);
    assert!(
        first
            .errors
            .iter()
            .any(|warning| warning.contains("statistics"))
    );
    assert_eq!(
        ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap()
            .skipped,
        1
    );
}

#[test]
fn planless_targets_are_skipped_without_reading_git() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    let mut target = target(&fixture);
    target.plans.clear();
    target.registration.common_dir = fixture.root.join("missing");
    let mut backend = TestBackend::default();
    let report = RepoIngestor::default()
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert_eq!(report.skipped, 1);
    assert!(report.errors.is_empty());
    assert_eq!(backend.requests, 0);
}
