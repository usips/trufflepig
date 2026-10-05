use super::super::board_backend::BoardBackend;
use super::super::board_protocol::{BoardOp, BoardRequest, FeedbackMetadata};
use super::super::board_vocabulary::{EntryText, FeedbackKind};
use super::super::{feedback_outbox, local_board::LocalBoard};
use super::*;

/// Seed a crossed-claim branch and ingest it, returning the linked oid.
fn ingest_crossed_claim() -> (EdgeFixture, String) {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(
        &["board", "new", "Git acceptance", "--steward", "codex"],
        "human",
        "owner",
    );
    fixture.run(&["board", "task", "P1", "Parser lane"], "human", "owner");
    fixture.run(
        &["board", "hello", "claude-opus", "high"],
        "claude",
        "claimant",
    );
    fixture.run(
        &["board", "claim", "P1.1", "Parser only; exclude review"],
        "claude",
        "claimant",
    );
    git(&fixture.root, &["switch", "-c", "non-current-lane"]);
    std::fs::write(fixture.root.join("lane.txt"), "crossed lane work\n").unwrap();
    git(&fixture.root, &["add", "lane.txt"]);
    git(
        &fixture.root,
        &[
            "commit",
            "-m",
            "Implement parser\n\nPlan: P1\nPlan-Task: P1.1\nCo-authored-by: Codex <noreply@openai.com>",
        ],
    );
    let linked_oid = git(&fixture.root, &["rev-parse", "HEAD"]);
    git(&fixture.root, &["switch", "main"]);
    assert_ne!(git(&fixture.root, &["rev-parse", "HEAD"]), linked_oid);
    fixture.run(&["board", "ingest"], "codex", "reviewer");
    (fixture, linked_oid)
}

#[test]
fn real_branch_ingestion_survives_host_reopening_and_review_identifies_crossed_claims() {
    let (fixture, linked_oid) = ingest_crossed_claim();
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 1);
    let events = fixture.scalar("SELECT COUNT(*) FROM events");
    let reopened = BoardHost::with_config(fixture.config.clone());
    reopened
        .run(
            &fixture.options(&["board", "ingest"]),
            &context("codex", "reviewer"),
            QueryDeadline::start(),
        )
        .unwrap();
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM events"),
        events,
        "fresh edge duplicated a durable commit event"
    );

    let review = fixture.run(&["board", "review", "P1@1", "codex"], "codex", "reviewer");
    let packet = review
        .get("review")
        .or_else(|| review.get("packet"))
        .unwrap_or_else(|| data(&review));
    let linked = packet["linked"].as_array().unwrap();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0]["oid"], linked_oid);
    assert_eq!(linked[0]["coauthors"][0]["harness"], "codex");
    let crossed = packet["crossed"].as_array().unwrap();
    assert_eq!(crossed.len(), 1);
    assert_eq!(crossed[0]["claimant"]["harness"], "claude");
    assert_eq!(crossed[0]["scope"], "Parser only; exclude review");
}

#[test]
#[cfg_attr(not(board_git_2_55), ignore = "requires Git >=2.55")]
fn real_branch_ingestion_survives_host_reopening_and_review_identifies_crossed_claims_drill_hint() {
    let (fixture, linked_oid) = ingest_crossed_claim();
    let review = fixture.run(&["board", "review", "P1@1", "codex"], "codex", "reviewer");
    let packet = review
        .get("review")
        .or_else(|| review.get("packet"))
        .unwrap_or_else(|| data(&review));
    let linked = packet["linked"].as_array().unwrap();
    assert_eq!(linked.len(), 1);
    assert!(linked[0]["drill"].as_str().unwrap().contains(&linked_oid));
}

#[test]
fn committed_outbox_aliases_remain_idempotent_after_manual_dedupe_expiry() {
    let fixture = EdgeFixture::new();
    let spool = fixture.scratch.path().join("outbox");
    let mut board = LocalBoard::open(&fixture.config).unwrap();
    let mut request = BoardRequest::new(
        fixture.actor("codex", "offline-session"),
        BoardOp::Feedback {
            kind: FeedbackKind::Blocked,
            summary: EntryText::new("The router was unavailable").unwrap(),
            body: Some(EntryText::new("Tried board; queued the report.").unwrap()),
            plan: None,
            metadata: FeedbackMetadata {
                version: "acceptance".into(),
                ..FeedbackMetadata::default()
            },
            import_key: Some(crate::board::board_vocabulary::FeedbackImportKey::new()),
        },
    );
    feedback_outbox::queue(&spool, &request).unwrap();
    let first = std::fs::read_dir(&spool)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let first_bytes = std::fs::read(&first).unwrap();
    assert_eq!(
        feedback_outbox::import_pending(&spool, &mut board)
            .unwrap()
            .imported,
        1
    );
    if let BoardOp::Feedback { import_key, .. } = &mut request.op {
        *import_key = Some(crate::board::board_vocabulary::FeedbackImportKey::new());
    }
    feedback_outbox::queue(&spool, &request).unwrap();
    let second = std::fs::read_dir(&spool)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let second_bytes = std::fs::read(&second).unwrap();
    assert_eq!(
        feedback_outbox::import_pending(&spool, &mut board)
            .unwrap()
            .imported,
        1
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM board_feedback"),
        1,
        "manual retry should share its existing feedback entry"
    );
    Connection::open(&fixture.config.db_path)
        .unwrap()
        .execute("DELETE FROM operation_dedupes", [])
        .unwrap();
    std::fs::write(&first, first_bytes).unwrap();
    std::fs::write(&second, second_bytes).unwrap();
    let replay = feedback_outbox::import_pending(&spool, &mut board).unwrap();
    assert_eq!(replay.imported, 2);
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM board_feedback"),
        1,
        "a committed UUID became eligible after ten-minute dedupe expiry"
    );
    assert_eq!(
        board.max_seq().unwrap().get(),
        1,
        "outbox replay minted an extra event"
    );
    assert!(!first.exists() && !second.exists());
}

#[test]
fn unrecognized_git_coauthor_ingests_and_reply_evidence_round_trips() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(&["board", "new", "Unrecognized coauthor"], "human", "owner");
    std::fs::write(fixture.root.join("coauthor.txt"), "authored evidence\n").unwrap();
    git(&fixture.root, &["add", "coauthor.txt"]);
    git(
        &fixture.root,
        &[
            "commit",
            "-m",
            "Record evidence\n\nPlan: P1\nCo-authored-by: Alice <alice@example.com>",
        ],
    );
    fixture.run(&["board", "ingest"], "codex", "reviewer");
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 1);
    let mut board = LocalBoard::open(&fixture.config).unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            fixture.config.actor(Some("codex"), Some("reader")).unwrap(),
            BoardOp::Show {
                target: "P1".parse().unwrap(),
            },
        ))
        .unwrap();
    let decoded: crate::board::board_protocol::BoardReply =
        serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
    let crate::board::board_protocol::BoardResult::Plan(view) = decoded.result else {
        panic!("expected plan view");
    };
    assert!(
        view.entries
            .iter()
            .any(|entry| entry.actor.harness.as_str() == "git:alice@example.com")
    );
}

#[test]
fn misplaced_trailers_surface_in_ingest_warnings_and_review_scan_errors() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(
        &["board", "new", "Misplaced trailers", "--steward", "codex"],
        "human",
        "owner",
    );
    fixture.run(&["board", "task", "P1", "Trailer lane"], "human", "owner");
    // The W5-H1 shape: blank lines between trailers, so Git parses only the
    // final paragraph (the co-author) and ignores both plan trailers.
    git(
        &fixture.root,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "Split trailer footer\n\nBody prose.\n\nPlan: P1\n\nPlan-Task: P1.1\n\nCo-authored-by: Muse Spark <noreply@meta.com>",
        ],
    );
    let oid = git(&fixture.root, &["rev-parse", "HEAD"]);
    let ingest = fixture.run(&["board", "ingest"], "codex", "reviewer");
    let warnings = ingest["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        warnings.iter().any(|warning| {
            let warning = warning.as_str().unwrap_or_default();
            warning.contains("misplaced_trailers") && warning.contains(&oid)
        }),
        "{warnings:?}"
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_plans"),
        0,
        "misplaced trailers never create links"
    );
    let review = fixture.run(&["board", "review", "P1@1"], "codex", "reviewer");
    let packet = data(&review);
    let scan_errors = packet["scan_errors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        scan_errors.iter().any(|error| {
            let error = error.as_str().unwrap_or_default();
            error.contains("misplaced_trailers") && error.contains(&oid)
        }),
        "{scan_errors:?}"
    );
}

#[test]
fn manual_link_repairs_an_untrailered_commit_through_host_resolution() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(
        &["board", "new", "Manual link", "--steward", "codex"],
        "human",
        "owner",
    );
    fixture.run(&["board", "task", "P1", "Repair lane"], "human", "owner");
    std::fs::write(fixture.root.join("repair.txt"), "untrailered work\n").unwrap();
    git(&fixture.root, &["add", "repair.txt"]);
    git(
        &fixture.root,
        &[
            "commit",
            "-m",
            "Repair by hand\n\nCo-authored-by: Model claim <noreply@openai.com>",
        ],
    );
    let oid = git(&fixture.root, &["rev-parse", "HEAD"]);
    fixture.run(&["board", "ingest"], "codex", "reviewer");
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_plans"),
        0,
        "an untrailered commit is never scan-linked"
    );
    let reply = fixture.run(&["board", "link", &oid, "P1.1"], "human", "owner");
    let change = data(&reply);
    assert_eq!(change["task"], "P1.1");
    assert_eq!(change["plan"], "P1");
    assert_eq!(change["deduplicated"], false);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 1);
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_plans WHERE source='manual'"),
        1
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_tasks WHERE source='manual'"),
        1
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commits WHERE subject='Repair by hand'"),
        1
    );
    let again = fixture.run(&["board", "link", &oid, "P1.1"], "human", "owner");
    let repeated = data(&again);
    assert_eq!(repeated["deduplicated"], true);
    assert_eq!(repeated["entry"], change["entry"]);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 1);
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM entries WHERE kind='commit'"),
        1,
        "a repeated manual link creates no duplicate entry"
    );
}

#[test]
fn manual_link_rejects_an_unknown_oid_before_writing() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(&["board", "new", "Manual link"], "human", "owner");
    fixture.run(&["board", "task", "P1", "Repair lane"], "human", "owner");
    let error = fixture
        .run_options(
            fixture.options(&["board", "link", &"f".repeat(40), "P1.1"]),
            "human",
            "owner",
            None,
        )
        .unwrap_err();
    assert!(
        error.to_string().starts_with("invalid_reference:"),
        "{error:#}"
    );
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 0);
}
