use super::*;

#[test]
fn cached_warning_tracks_current_task_links() {
    let fixture = GitFixture::new();
    let oid = fixture
        .commit("Split footer\n\nPlan: P7\n\nCo-authored-by: Model claim <noreply@openai.com>");
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
    assert_eq!(first.errors.len(), 1);
    let key = (target.registration.repo_key.clone(), oid);
    backend.manual_oids.insert(key.clone());
    let linked = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[target.clone()],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(linked.skipped, 1);
    assert!(linked.errors.is_empty());
    backend.manual_oids.remove(&key);
    let unlinked = ingestor
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert_eq!(unlinked.skipped, 1);
    assert_eq!(
        unlinked.errors, first.errors,
        "the cache retains the original warning for an unlink"
    );
}

#[test]
fn per_repo_suppression_keeps_unrelated_warnings_and_foreign_same_oid() {
    let repo = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let other_repo = RepoKey::from_roots([GitOid::parse(&"b".repeat(40)).unwrap()]).unwrap();
    let oid = GitOid::parse(&"c".repeat(40)).unwrap();
    let mut backend = TestBackend::default();
    backend.manual_oids.insert((repo.clone(), oid));
    let warning = format!("board_scan: {oid}: misplaced_trailers");
    let unrelated = format!("board_scan: stats unavailable while inspecting {oid}");
    let mut warnings = vec![warning.clone(), unrelated.clone()];
    suppress_linked_warnings(&backend, &repo, &mut warnings).unwrap();
    assert_eq!(warnings, vec![unrelated.clone()]);
    let mut foreign = vec![warning.clone(), unrelated.clone()];
    suppress_linked_warnings(&backend, &other_repo, &mut foreign).unwrap();
    assert_eq!(foreign, vec![warning, unrelated]);
}
