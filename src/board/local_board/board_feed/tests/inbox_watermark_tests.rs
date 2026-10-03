use super::*;

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
