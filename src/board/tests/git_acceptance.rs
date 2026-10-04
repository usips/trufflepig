use super::super::board_backend::BoardBackend;
use super::super::board_protocol::{BoardOp, BoardRequest, FeedbackMetadata};
use super::super::board_vocabulary::{EntryText, FeedbackKind};
use super::super::{feedback_outbox, local_board::LocalBoard};
use super::*;

#[test]
fn real_branch_ingestion_survives_host_reopening_and_review_identifies_crossed_claims() {
    if crate::board::board_test_support::git_version() < Some((2, 55)) {
        eprintln!(
            "skipping real_branch_ingestion_survives_host_reopening_and_review_identifies_crossed_claims: requires Git >= 2.55 for history drill hints"
        );
        return;
    }
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
    assert!(linked[0]["drill"].as_str().unwrap().contains(&linked_oid));
    let crossed = packet["crossed"].as_array().unwrap();
    assert_eq!(crossed.len(), 1);
    assert_eq!(crossed[0]["claimant"]["harness"], "claude");
    assert_eq!(crossed[0]["scope"], "Parser only; exclude review");
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
