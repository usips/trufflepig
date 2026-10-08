use super::*;
use crate::board::board_protocol::ReadScope;

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
    let actor_id = crate::board::local_board::lookup_actor(&tx, &actor)
        .unwrap()
        .unwrap();
    let ctx = crate::board::local_board::WriteContext {
        actor_id,
        actor,
        model: Some("event model".into()),
        effort: Some("event effort".into()),
        now: 30,
        seq: EventSeq::new(2),
        claim_ttl_secs: 120,
        via: None,
    };
    crate::board::local_board::insert_event(
        &tx,
        &ctx,
        Some(plan),
        EntryKind::Note,
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
                target: BoardRef::Plan(plan),
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
        &ReadScope::All,
        20,
    )
    .unwrap();
    assert_eq!(events[0].via, Some(FeedbackVia::Outbox));
    assert_eq!(events[1].via, None);
}
