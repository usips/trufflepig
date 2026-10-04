use super::*;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn native_trailers_link_case_insensitively_and_exclude_body_mentions() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    fixture.commit("body mention\n\nPlan: P7\n\nThis is body prose, not a trailer.");
    std::fs::write(fixture.root.join("source.txt"), "one\ntwo\n").unwrap();
    fixture.git(&["add", "source.txt"]);
    let linked = fixture.commit("real footer\n\nPLAN: P7\nPlan-Task: P7.3\nCo-Authored-By: Model Claim <noreply@OpenAI.com>");
    let folded = fixture.commit("folded footer\n\nPlan:\n P7\nPlan-Task:\n P7.3");
    let spaced = fixture.commit("spaced footer\n\nPlan : P7");
    let target = target(&fixture);
    let mut backend = TestBackend::default();
    let report = RepoIngestor::default()
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
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
    assert_eq!(backend.linked.len(), 1);
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
    std::fs::remove_file(
        target
            .registration
            .common_dir
            .join(format!("objects/{}/{}", &tree[..2], &tree[2..])),
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
        writeln!(stream, "commit refs/heads/main\ncommitter Fixture <fixture@example.test> {} +0000\ndata {}\n{}\n", 1_700_000_000 + ordinal, message.len(), message).unwrap();
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
