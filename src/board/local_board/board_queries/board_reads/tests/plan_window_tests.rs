use super::*;

#[test]
fn plan_views_keep_old_open_questions_and_resolve_references_to_answers() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let question = post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "Should this work?",
    );
    for index in 0..25 {
        post(
            &mut board,
            "codex",
            plan,
            EntryKind::Progress,
            &format!("update {index}"),
        );
    }
    call(
        &mut board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Covered task").unwrap(),
            to: None,
            section: Some("Covered".to_owned()),
        },
    );
    let BoardResult::Plan(view) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
    ) else {
        panic!("missing plan")
    };
    assert_eq!(view.entries.len(), 21);
    assert!(view.entries.iter().any(|entry| entry.id == question));
    assert_eq!(view.entries_next_before, None);
    assert!(
        view.entries
            .windows(2)
            .all(|pair| (pair[0].seq, pair[0].id) > (pair[1].seq, pair[1].id)),
        "plan windows read newest-first"
    );
    assert_eq!(view.sections_without_tasks, ["Uncovered"]);
    let answer = post(
        &mut board,
        "codex",
        plan,
        EntryKind::Answer,
        &format!("Yes: {question}"),
    );
    assert_eq!(
        entry(&board.conn, answer).unwrap().refs,
        [BoardRef::Entry(question)]
    );
    let BoardResult::Plan(view) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
    ) else {
        panic!("missing plan")
    };
    assert_eq!(view.entries.len(), 20);
    assert!(!view.entries.iter().any(|entry| entry.id == question));
}

#[test]
fn plan_windows_omit_oldest_entries_behind_a_before_cursor() {
    let (_directory, board) = database();
    board
        .conn
        .execute_batch(
            "INSERT INTO actors VALUES(1,'josh','laptop','codex','s1');
             INSERT INTO plans(id,title,owner_user,head_revision,created_at)
             VALUES(1,'One','josh',1,1);
             INSERT INTO texts VALUES('one','body');
             INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
             VALUES(1,1,'create','One',1,1,50);
             INSERT INTO revisions VALUES(1,1,'one','create',1,1,1);",
        )
        .unwrap();
    for number in 2..=202u64 {
        board
            .conn
            .execute(
                "INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at) \
                 VALUES(?1,1,'question','open?',1,?2,50)",
                params![number as i64, number as i64],
            )
            .unwrap();
    }
    let ctx = WriteContext {
        actor_id: 1,
        actor: BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse("codex").unwrap(),
            "session1",
        )
        .unwrap(),
        model: None,
        effort: None,
        now: 100,
        seq: EventSeq::new(0),
        claim_ttl_secs: 20,
        via: None,
    };
    let view = plan_view(&board.conn, &ctx, PlanId::new(1).unwrap()).unwrap();
    assert_eq!(view.entries.len(), 200);
    assert_eq!(view.entries_omitted, 1);
    assert_eq!(view.entries.first().unwrap().id.get(), 202);
    assert_eq!(view.entries.last().unwrap().id.get(), 3);
    assert_eq!(
        view.entries_next_before,
        Some(EntryCursor {
            seq: EventSeq::new(183),
            entry: EntryId::new(183).unwrap()
        })
    );
}

#[test]
fn plan_view_pages_past_open_extras_from_the_recent_window() {
    let (_directory, mut board) = database();
    board
        .conn
        .execute_batch(
            "INSERT INTO actors VALUES(1,'josh','laptop','codex','s1');
             INSERT INTO plans(id,title,owner_user,head_revision,created_at)
             VALUES(1,'One','josh',1,1);
             INSERT INTO texts VALUES('one','body');
             INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
             VALUES(1,1,'create','One',1,1,50);
             INSERT INTO revisions VALUES(1,1,'one','create',1,1,1);
             INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at)
             VALUES(221,1,'note','E221',1,'seeded',50);",
        )
        .unwrap();
    for number in 2..=191u64 {
        board
            .conn
            .execute(
                "INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at) \
                 VALUES(?1,1,'question','open?',1,?2,50)",
                params![number as i64, number as i64],
            )
            .unwrap();
    }
    for number in 192..=221u64 {
        board
            .conn
            .execute(
                "INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at) \
                 VALUES(?1,1,'note','field note',1,?2,50)",
                params![number as i64, number as i64],
            )
            .unwrap();
    }
    let ctx = WriteContext {
        actor_id: 1,
        actor: BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse("codex").unwrap(),
            "session1",
        )
        .unwrap(),
        model: None,
        effort: None,
        now: 100,
        seq: EventSeq::new(0),
        claim_ttl_secs: 20,
        via: None,
    };
    let plan = PlanId::new(1).unwrap();
    let view = plan_view(&board.conn, &ctx, plan).unwrap();
    assert_eq!(view.entries.len(), 200);
    assert_eq!(view.entries_omitted, 10);
    assert_eq!(
        view.entries_next_before,
        Some(EntryCursor {
            seq: EventSeq::new(202),
            entry: EntryId::new(202).unwrap()
        })
    );
    assert!(
        view.entries
            .iter()
            .any(|entry| entry.id == EntryId::new(191).unwrap()),
        "old open questions stay listed past the recent window"
    );
    assert!(
        view.entries
            .windows(2)
            .all(|pair| (pair[0].seq, pair[0].id) > (pair[1].seq, pair[1].id)),
        "plan windows read newest-first"
    );
    let BoardResult::Entries(page) = call(
        &mut board,
        "codex",
        BoardOp::Entries {
            plan: Some(plan),
            kind: None,
            harness: None,
            user: None,
            host: None,
            task: None,
            references: None,
            after: None,
            before: view.entries_next_before,
            through: None,
            limit: 5,
        },
    ) else {
        panic!("missing entries page")
    };
    assert_eq!(page.entries.first().unwrap().id.get(), 201);
}

#[test]
fn plan_view_omits_the_retired_after_cursor() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let BoardResult::Plan(view) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
    ) else {
        panic!("missing plan")
    };
    let value = serde_json::to_value(&view).unwrap();
    assert!(
        !value
            .as_object()
            .unwrap()
            .contains_key("entries_next_after"),
        "plan views page entries newest-first with entries_next_before only"
    );
}
