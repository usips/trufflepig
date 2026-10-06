use super::*;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn commit_graph_warning_keeps_links_and_completes_scan() {
    let fixture = GitFixture::new();
    let linked = fixture.commit(&linked_message("linked"));
    let target = target(&fixture);
    std::fs::write(
        target
            .registration
            .common_dir
            .join("objects/info/commit-graph"),
        b"corrupt",
    )
    .unwrap();
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
        backend.linked.iter().any(|commit| commit.oid == linked),
        "per-commit Git warnings must not block linking: {:?}",
        report.errors
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
fn stats_failure_keeps_links_and_reports_warning() {
    let fixture = GitFixture::new();
    std::fs::write(fixture.root.join("source.txt"), "data\n").unwrap();
    fixture.git(&["add", "source.txt"]);
    fixture.commit("file commit");
    let linked = fixture.commit(&linked_message("linked"));
    let target = target(&fixture);
    let tree = fixture.git(&["rev-parse", "HEAD~1^{tree}"]);
    let tree = tree.trim();
    std::fs::remove_file(target.registration.common_dir.join(format!(
        "objects/{}/{}",
        &tree[..2],
        &tree[2..]
    )))
    .unwrap();
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
            .any(|error| error.contains("commit statistics unavailable")),
        "{:?}",
        report.errors
    );
    let commit = backend
        .linked
        .iter()
        .find(|commit| commit.oid == linked)
        .expect("stats failures must not drop the plan link");
    assert_eq!(commit.files, 0);
    assert_eq!(
        ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap()
            .skipped,
        1
    );
}

#[test]
fn unlinked_lookup_filters_by_vendor_and_true_plan_trailer_presence() {
    let fixture = GitFixture::new();
    fixture.commit("human root");
    let unlinked = fixture.commit("unlinked\n\nCo-authored-by: Codex claim <noreply@openai.com>");
    fixture.commit(&linked_message("linked"));
    fixture.commit("other vendor\n\nCo-authored-by: Claude claim <noreply@anthropic.com>");
    fixture.commit(
        "invalid link\n\nPlan: not-a-plan\nCo-authored-by: Codex claim <noreply@openai.com>",
    );
    let task_only = fixture
        .commit("task only\n\nPlan-Task: P7.3\nCo-authored-by: Codex claim <noreply@openai.com>");
    let scan = find_unlinked(
        &fixture.registration(),
        1_700_000_000,
        &HarnessLabel::parse("codex").unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(scan.complete);
    assert_eq!(scan.commits.len(), 2);
    assert_eq!(
        scan.commits
            .iter()
            .map(|commit| commit.oid)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([unlinked, task_only])
    );
}

#[test]
fn capped_scan_links_bounded_records_without_advancing_digest() {
    let fixture = GitFixture::new();
    let message = linked_message("bulk linked");
    let mut stream = Vec::new();
    for ordinal in 0..=COMMIT_LIMIT {
        writeln!(
            stream,
            "commit refs/heads/main\ncommitter Fixture <fixture@example.test> {} +0000\ndata {}\n{}\n",
            1_700_000_000 + ordinal,
            message.len(),
            message
        )
        .unwrap();
    }
    let mut child = Command::new("git")
        .current_dir(&fixture.root)
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&stream).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = target(&fixture);
    let mut ingestor = RepoIngestor::default();
    let mut backend = TestBackend::default();
    for _ in 0..2 {
        let report = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[target.clone()],
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(report.inserted, COMMIT_LIMIT as u64);
        assert_eq!(report.errors.len(), 1);
    }
    assert_eq!(backend.requests, 2);
    assert!(ingestor.completed.is_empty());
}

#[test]
fn scan_survives_long_bodies() {
    let fixture = GitFixture::new();
    let body = "lorem ipsum dolor sit amet, consectetur adipiscing elit\n".repeat(100);
    assert!(body.len() >= 5 * 1024);
    let mut stream = Vec::new();
    for ordinal in 0..=COMMIT_LIMIT {
        let message = if ordinal == 0 {
            "root\n".to_owned()
        } else {
            format!("bulk {ordinal}\n\n{body}\nPlan: P7\n")
        };
        writeln!(
            stream,
            "commit refs/heads/main\ncommitter Fixture <fixture@example.test> {} +0000\ndata {}\n{}\n",
            1_700_000_100 + ordinal as i64,
            message.len(),
            message
        )
        .unwrap();
    }
    let mut child = Command::new("git")
        .current_dir(&fixture.root)
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&stream).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let registration = fixture.registration();
    let deadline = Instant::now() + Duration::from_secs(30);
    let tips = commit_scanner::collect_tips(&registration, deadline).unwrap();
    let scan =
        commit_scanner::scan_log(&registration, &tips, 1_700_000_000, true, deadline).unwrap();
    assert!(
        scan.complete,
        "a full window of long-body commits must scan completely"
    );
    assert_eq!(scan.records.len(), COMMIT_LIMIT);
}

#[test]
fn broken_git_reference_warning_prevents_complete_scan() {
    let fixture = GitFixture::new();
    fixture.commit(&linked_message("valid root"));
    let target = target(&fixture);
    std::fs::write(
        target.registration.common_dir.join("refs/heads/broken"),
        "not-an-oid\n",
    )
    .unwrap();
    let mut backend = TestBackend::default();
    let mut ingestor = RepoIngestor::default();
    let report = ingestor
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert_eq!(backend.requests, 0);
    assert!(report.errors[0].contains("warning prevents a complete scan"));
    assert!(ingestor.completed.is_empty());
}
