use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::TaskId;
use crate::board::board_protocol::{BoardOp, BoardRequest};
use crate::board::board_vocabulary::{EntryKind, PlanText, PlanTitle};
use crate::board::local_board::LocalBoard;
use std::time::Duration;

fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = crate::board::board_test_support::scratch("board-test-");
    let board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(7200),
    )
    .unwrap();
    (directory, board)
}

fn call(board: &mut LocalBoard, harness: &str, op: BoardOp) -> BoardResult {
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        "session1",
    )
    .unwrap();
    board.handle(&BoardRequest::new(actor, op)).unwrap().result
}

fn plan(board: &mut LocalBoard) -> PlanId {
    match call(
        board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Trial").unwrap(),
            body: PlanText::new("# Scope").unwrap(),
            steward: None,
        },
    ) {
        BoardResult::Change(change) => change.plan.unwrap(),
        other => panic!("unexpected {other:?}"),
    }
}

fn post(
    board: &mut LocalBoard,
    harness: &str,
    plan: PlanId,
    kind: EntryKind,
    body: &str,
    to: Option<&str>,
) {
    call(
        board,
        harness,
        BoardOp::Post {
            target: BoardRef::Plan(plan),
            kind,
            body: EntryText::new(body).unwrap(),
            to: to.map(|value| BoardRecipient::parse(value).unwrap()),
            supersedes: None,
        },
    );
}

fn feed(board: &mut LocalBoard, after: Option<EventSeq>, limit: usize) -> InboxReply {
    match call(
        board,
        "codex",
        BoardOp::Inbox {
            after,
            limit,
            repo_key: None,
            all: true,
        },
    ) {
        BoardResult::Inbox(inbox) => inbox,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn first_feed_keeps_twenty_fresh_events_and_old_reminders_without_advancing() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "old unanswered question",
        None,
    );
    for index in 0..25 {
        post(
            &mut board,
            "claude",
            plan,
            EntryKind::Progress,
            &format!("progress {index}"),
            None,
        );
    }
    post(
        &mut board,
        "codex",
        plan,
        EntryKind::Note,
        "my own write",
        None,
    );
    let first = feed(&mut board, None, 100);
    assert_eq!(first.events.len(), 20);
    assert_eq!(first.events.first().unwrap().seq.get(), 8);
    assert_eq!(first.events.last().unwrap().seq.get(), 27);
    assert_eq!(first.open.len(), 1);
    assert_eq!(first.open[0].seq.get(), 2);
    assert_eq!(first.cursor.get(), 0);
    assert_eq!(feed(&mut board, None, 100).events, first.events);
    let through = first.events[4].seq;
    assert_eq!(
        call(
            &mut board,
            "codex",
            BoardOp::AcknowledgeInbox {
                rendered_through: through
            }
        ),
        BoardResult::Cursor(through)
    );
    let next = feed(&mut board, None, 100);
    assert_eq!(next.events, first.events[5..]);
    assert_eq!(next.open, first.open);
    let explicit = feed(&mut board, Some(EventSeq::new(0)), 100);
    assert!(!explicit.advancing);
    assert_eq!(explicit.cursor, through);
    assert_eq!(explicit.events.len(), 27);
    assert_eq!(
        call(
            &mut board,
            "codex",
            BoardOp::AcknowledgeInbox {
                rendered_through: EventSeq::new(1)
            }
        ),
        BoardResult::Cursor(through)
    );
}

#[test]
fn addressed_news_filters_recipients_while_labor_and_own_reminders_remain_visible() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "private question for muse",
        Some("muse"),
    );
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Note,
        "for codex",
        Some("codex"),
    );
    post(
        &mut board,
        "codex",
        plan,
        EntryKind::Question,
        "my unresolved question",
        None,
    );
    call(
        &mut board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Shared labor").unwrap(),
            to: Some(BoardRecipient::parse("muse").unwrap()),
            section: None,
        },
    );
    let inbox = feed(&mut board, Some(EventSeq::new(0)), 100);
    assert!(
        !inbox
            .events
            .iter()
            .any(|event| event.summary.as_str().contains("private question"))
    );
    assert!(
        inbox
            .events
            .iter()
            .any(|event| event.summary.as_str().contains("for codex"))
    );
    assert!(
        inbox
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Task)
    );
    assert!(
        !inbox
            .events
            .iter()
            .any(|event| event.actor.harness.as_str() == "codex")
    );
    assert_eq!(inbox.open.len(), 1);
    assert_eq!(inbox.open[0].actor.harness.as_str(), "codex");
    assert!(
        feed(&mut board, None, 1)
            .open
            .iter()
            .any(|entry| entry.body.as_str() == "my unresolved question")
    );
}

#[test]
fn inbox_refreshes_held_leases_without_a_housekeeping_event() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    call(
        &mut board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Lane").unwrap(),
            to: None,
            section: None,
        },
    );
    call(
        &mut board,
        "codex",
        BoardOp::ClaimTask {
            task: TaskId::new(plan, 1).unwrap(),
            scope: Some(EntryText::new("parser and tests").unwrap()),
            resume: false,
        },
    );
    board
        .conn
        .execute("UPDATE claims SET last_active=0", [])
        .unwrap();
    let before = board.max_seq().unwrap();
    feed(&mut board, None, 20);
    let active: i64 = board
        .conn
        .query_row("SELECT last_active FROM claims", [], |row| row.get(0))
        .unwrap();
    assert!(active > 0);
    assert_eq!(board.max_seq().unwrap(), before);
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "session1",
    )
    .unwrap();
    let error = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::AcknowledgeInbox {
                rendered_through: EventSeq::new(before.get() + 1),
            },
        ))
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidReference
    );
}

fn scoped_feed(
    board: &mut LocalBoard,
    repo_key: Option<RepoKey>,
    all: bool,
    limit: usize,
) -> InboxReply {
    match call(
        board,
        "codex",
        BoardOp::Inbox {
            after: Some(EventSeq::new(0)),
            limit,
            repo_key,
            all,
        },
    ) {
        BoardResult::Inbox(inbox) => inbox,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn inbox_scope_keeps_repo_news_addressed_messages_and_own_feedback_outcomes() {
    use crate::board::board_vocabulary::{FeedbackKind, FeedbackState};
    let (_directory, mut board) = database();
    let first = plan(&mut board);
    let BoardResult::Change(second) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Another repository").unwrap(),
            body: PlanText::new("").unwrap(),
            steward: None,
        },
    ) else {
        panic!("missing plan");
    };
    let second = second.plan.unwrap();
    let repo = RepoKey::from_roots([crate::identity::GitOid::parse(
        "1111111111111111111111111111111111111111",
    )
    .unwrap()])
    .unwrap();
    board
        .conn
        .execute("INSERT INTO repos(repo_key) VALUES(?1)", [repo.as_str()])
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
            params![sql_number(first.get()), repo.as_str()],
        )
        .unwrap();
    post(
        &mut board,
        "claude",
        first,
        EntryKind::Note,
        "caller repo news",
        None,
    );
    post(
        &mut board,
        "claude",
        second,
        EntryKind::Note,
        "foreign repo news",
        None,
    );
    post(
        &mut board,
        "claude",
        second,
        EntryKind::Note,
        "foreign direct message",
        Some("codex"),
    );
    post(
        &mut board,
        "claude",
        first,
        EntryKind::Question,
        "caller repo reminder",
        None,
    );
    post(
        &mut board,
        "claude",
        second,
        EntryKind::Question,
        "foreign reminder",
        None,
    );
    let BoardResult::Change(report) = call(
        &mut board,
        "codex",
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("my report").unwrap(),
            body: None,
            plan: None,
            metadata: crate::board::board_protocol::FeedbackMetadata::default(),
            import_key: None,
        },
    ) else {
        panic!("missing report");
    };
    call(
        &mut board,
        "human",
        BoardOp::FeedbackClose {
            entry: report.entry,
            state: FeedbackState::Fixed,
            note: None,
        },
    );
    let inbox = scoped_feed(&mut board, Some(repo.clone()), false, 100);
    assert!(
        inbox
            .events
            .iter()
            .any(|event| event.summary.as_str() == "caller repo news")
    );
    assert!(
        !inbox
            .events
            .iter()
            .any(|event| event.summary.as_str() == "foreign repo news")
    );
    assert!(
        inbox
            .events
            .iter()
            .any(|event| event.summary.as_str() == "foreign direct message")
    );
    assert!(
        inbox
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Feedback
                && event.subject == BoardRef::Entry(report.entry))
    );
    assert_eq!(inbox.open.len(), 1);
    assert_eq!(inbox.open[0].body.as_str(), "caller repo reminder");
    let all = scoped_feed(&mut board, Some(repo), true, 100);
    assert!(
        all.events
            .iter()
            .any(|event| event.summary.as_str() == "foreign repo news")
    );
    assert_eq!(all.open.len(), 2);
    let without_repo = scoped_feed(&mut board, None, false, 100);
    assert!(
        !without_repo
            .events
            .iter()
            .any(|event| event.summary.as_str() == "caller repo news")
    );
    assert!(
        without_repo
            .events
            .iter()
            .any(|event| event.summary.as_str() == "foreign direct message")
    );
}

#[test]
fn inbox_reminder_query_caps_materialization_and_reports_omissions() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    for index in 0..30 {
        post(
            &mut board,
            "claude",
            plan,
            EntryKind::Question,
            &format!("question {index}"),
            None,
        );
    }
    let bounded = scoped_feed(&mut board, None, true, 2);
    assert_eq!(bounded.open.len(), 2);
    assert_eq!(bounded.open_omitted, 28);
    let capped = scoped_feed(&mut board, None, true, 100);
    assert_eq!(capped.open.len(), 20);
    assert_eq!(capped.open_omitted, 10);
}

#[test]
fn mixed_plan_commit_event_is_visible_via_its_same_sequence_entries() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let repo = RepoKey::from_roots([crate::identity::GitOid::parse(
        "2222222222222222222222222222222222222222",
    )
    .unwrap()])
    .unwrap();
    board
        .conn
        .execute("INSERT INTO repos(repo_key) VALUES(?1)", [repo.as_str()])
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO plan_repos VALUES(?1,?2)",
            params![sql_number(plan.get()), repo.as_str()],
        )
        .unwrap();
    let seq = board.max_seq().unwrap().get() + 1;
    let actor_id: i64 = board
        .conn
        .query_row("SELECT id FROM actors WHERE harness='human'", [], |row| {
            row.get(0)
        })
        .unwrap();
    board.conn.execute("INSERT INTO entries(plan_id,kind,body,actor_id,repo_key,seq,created_at) VALUES(?1,'commit','linked batch',?2,?3,?4,0)",params![sql_number(plan.get()),actor_id,repo.as_str(),sql_number(seq)]).unwrap();
    board.conn.execute("INSERT INTO events(seq,kind,subject,actor_id,summary,created_at) VALUES(?1,'commit','E1',?2,'linked mixed plans',0)",params![sql_number(seq),actor_id]).unwrap();
    let inbox = scoped_feed(&mut board, Some(repo), false, 100);
    assert!(inbox.events.iter().any(|event| event.seq.get() == seq));
}

#[test]
fn scan_watermark_skips_own_and_irrelevant_events_but_preserves_next_foreign_event() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    post(
        &mut board,
        "codex",
        plan,
        EntryKind::Note,
        "own first",
        None,
    );
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Note,
        "foreign first",
        None,
    );
    post(
        &mut board,
        "codex",
        plan,
        EntryKind::Note,
        "own middle",
        None,
    );
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Note,
        "irrelevant recipient",
        Some("muse"),
    );
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Note,
        "foreign next",
        None,
    );
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Note,
        "foreign last",
        None,
    );
    feed(&mut board, None, 100);
    call(
        &mut board,
        "codex",
        BoardOp::AcknowledgeInbox {
            rendered_through: EventSeq::new(1),
        },
    );
    let first = feed(&mut board, None, 1);
    assert_eq!(first.events[0].seq.get(), 3);
    assert_eq!(first.scanned_through.get(), 5);
    assert!(first.query_truncated);
    let rendered = crate::board::board_render::render_reply(
        &BoardReply::new("local", BoardResult::Inbox(first.clone())),
        &crate::output::OutputBudget::new(2000).unwrap(),
    )
    .unwrap();
    assert_eq!(rendered.acknowledge_seq, Some(EventSeq::new(3)));
    call(
        &mut board,
        "codex",
        BoardOp::AcknowledgeInbox {
            rendered_through: rendered.acknowledge_seq.unwrap(),
        },
    );
    let next = feed(&mut board, None, 100);
    assert_eq!(next.events[0].seq.get(), 6);
    assert_eq!(next.scanned_through.get(), 7);
    let explicit = feed(&mut board, Some(EventSeq::new(7)), 100);
    assert!(!explicit.advancing);
    assert!(explicit.events.is_empty());
    post(
        &mut board,
        "codex",
        plan,
        EntryKind::Note,
        "own only tail",
        None,
    );
    call(
        &mut board,
        "codex",
        BoardOp::AcknowledgeInbox {
            rendered_through: EventSeq::new(7),
        },
    );
    let own_only = feed(&mut board, None, 100);
    assert!(own_only.events.is_empty());
    assert_eq!(own_only.scanned_through.get(), 8);
}

#[test]
fn first_inbox_limit_selects_a_recent_seed_not_a_forward_page() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    for index in 0..6 {
        post(
            &mut board,
            "claude",
            plan,
            EntryKind::Note,
            &format!("seed fact {index}"),
            None,
        );
    }
    let seed = feed(&mut board, None, 2);
    assert_eq!(
        seed.events
            .iter()
            .map(|event| event.seq.get())
            .collect::<Vec<_>>(),
        vec![6, 7]
    );
    assert!(!seed.query_truncated);
    assert_eq!(seed.scanned_through.get(), 7);
    assert_eq!(seed.cursor.get(), 0);
}

#[test]
fn event_batches_return_stored_claim_snapshots_without_entry_joins() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("human").unwrap(),
        "session1",
    )
    .unwrap();
    let tx = board.conn.transaction().unwrap();
    let actor_id = super::super::lookup_actor(&tx, &actor).unwrap().unwrap();
    let ctx = super::super::WriteContext {
        actor_id,
        actor,
        model: Some("event model".into()),
        effort: Some("event effort".into()),
        now: 30,
        seq: EventSeq::new(2),
        claim_ttl_secs: 120,
        via: None,
    };
    super::super::insert_event(
        &tx,
        &ctx,
        Some(plan),
        "note",
        &plan.to_string(),
        None,
        "event without an entry",
    )
    .unwrap();
    tx.commit().unwrap();
    board
        .conn
        .execute(
            "UPDATE agent_sessions SET model='later model',effort='later effort'",
            [],
        )
        .unwrap();
    let (latest, events) = board
        .read_event_batch(EventSeq::new(1), Some(plan), 501)
        .unwrap();
    assert_eq!(latest.get(), 2);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].model.as_deref(), Some("event model"));
    assert_eq!(events[0].effort.as_deref(), Some("event effort"));
    assert_eq!(board.reader.as_ref().unwrap().total_changes(), 0);
    let reader = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "unknown-reader",
    )
    .unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            reader,
            BoardOp::Show {
                target: Some(BoardRef::Plan(plan)),
            },
        ))
        .unwrap();
    assert_eq!(reply.snapshot_seq, Some(latest));
    assert!(
        board
            .read_event_batch(EventSeq::new(0), Some(plan), 502)
            .is_err()
    );
}

#[test]
fn imported_feedback_provenance_reaches_normal_inbox_and_shared_event_feed() {
    use crate::board::board_protocol::{FeedbackMetadata, FeedbackVia};
    use crate::board::board_vocabulary::{FeedbackImportKey, FeedbackKind};
    let (_directory, mut board) = database();
    let reported = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("human").unwrap(),
        "spooled-claim",
    )
    .unwrap();
    let imported = BoardRequest::new(
        reported.clone(),
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("spooled human identity claim").unwrap(),
            body: None,
            plan: None,
            metadata: FeedbackMetadata::default(),
            import_key: Some(FeedbackImportKey::new()),
        },
    );
    board.import_feedback(&imported).unwrap();
    let mut live = imported.clone();
    let BoardOp::Feedback {
        summary,
        import_key,
        ..
    } = &mut live.op
    else {
        unreachable!()
    };
    *summary = EntryText::new("live feedback with a permanent key").unwrap();
    *import_key = Some(FeedbackImportKey::new());
    board.handle(&live).unwrap();
    let inbox = feed(&mut board, Some(EventSeq::new(0)), 20);
    assert_eq!(inbox.events.len(), 2);
    assert_eq!(inbox.events[0].via, Some(FeedbackVia::Outbox));
    assert_eq!(inbox.events[1].via, None);
    assert_eq!(
        serde_json::to_value(&inbox.events[0]).unwrap()["via"],
        "outbox"
    );
    let reply = BoardReply::new("test", BoardResult::Inbox(inbox));
    let lines = crate::board::board_render::render_reply(
        &reply,
        &crate::output::OutputBudget::new(4000)
            .unwrap()
            .with_format(crate::output::OutputFormat::Lines),
    )
    .unwrap()
    .text;
    let imported_line = lines
        .lines()
        .find(|line| line.contains("spooled human identity claim"))
        .unwrap();
    assert!(imported_line.contains("via=outbox spooled unverified"));
    let live_line = lines
        .lines()
        .find(|line| line.contains("live feedback with a permanent key"))
        .unwrap();
    assert!(!live_line.contains("spooled unverified"));
    let events = read_events(
        &board.conn,
        EventSeq::new(0),
        board.max_seq().unwrap(),
        None,
        20,
    )
    .unwrap();
    assert_eq!(events[0].via, Some(FeedbackVia::Outbox));
    assert_eq!(events[1].via, None);
}
