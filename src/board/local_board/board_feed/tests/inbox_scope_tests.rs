use super::*;

#[test]
fn inbox_scope_keeps_repo_news_addressed_messages_and_own_feedback_outcomes() {
    use crate::board::board_vocabulary::{FeedbackKind, FeedbackState};
    let (_directory, mut board) = database();
    let first = plan(&mut board);
    let BoardResult::Change(second) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Unlinked plan").unwrap(),
            body: PlanText::new("").unwrap(),
            steward: None,
            repo_key: None,
        },
    ) else {
        panic!("missing plan");
    };
    let second = second.plan.unwrap();
    let BoardResult::Change(third) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Foreign repository plan").unwrap(),
            body: PlanText::new("").unwrap(),
            steward: None,
            repo_key: None,
        },
    ) else {
        panic!("missing plan");
    };
    let third = third.plan.unwrap();
    let repo = RepoKey::from_roots([crate::identity::GitOid::parse(
        "1111111111111111111111111111111111111111",
    )
    .unwrap()])
    .unwrap();
    let foreign = RepoKey::from_roots([crate::identity::GitOid::parse(
        "3333333333333333333333333333333333333333",
    )
    .unwrap()])
    .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO repos(repo_key) VALUES(?1),(?2)",
            [repo.as_str(), foreign.as_str()],
        )
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
            params![sql_number(first.get()), repo.as_str()],
        )
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
            params![sql_number(third.get()), foreign.as_str()],
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
        "unlinked plan news",
        None,
    );
    post(
        &mut board,
        "claude",
        third,
        EntryKind::Note,
        "foreign repo news",
        None,
    );
    post(
        &mut board,
        "claude",
        third,
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
        "unlinked reminder",
        None,
    );
    post(
        &mut board,
        "claude",
        third,
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
        inbox
            .events
            .iter()
            .any(|event| event.summary.as_str() == "unlinked plan news"),
        "a plan with no repository link is global and appears in every scope"
    );
    assert!(
        inbox.events.iter().any(|event| event.kind == EntryKind::Create
            && event.plan == Some(second)),
        "an unlinked plan's creation is visible in every scoped inbox"
    );
    assert!(
        !inbox
            .events
            .iter()
            .any(|event| event.summary.as_str() == "foreign repo news"),
        "a plan linked to another repository must not leak into this scope"
    );
    assert!(
        !inbox.events.iter().any(|event| event.kind == EntryKind::Create
            && event.plan == Some(third))
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
    assert_eq!(inbox.open.len(), 2);
    assert_eq!(inbox.open[0].body.as_str(), "caller repo reminder");
    assert_eq!(inbox.open[1].body.as_str(), "unlinked reminder");
    let all = scoped_feed(&mut board, Some(repo), true, 100);
    assert!(
        all.events
            .iter()
            .any(|event| event.summary.as_str() == "foreign repo news")
    );
    assert_eq!(all.open.len(), 3);
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
            .any(|event| event.summary.as_str() == "unlinked plan news")
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
fn inbox_reminder_count_is_bounded_and_reports_capped_omissions() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    for index in 0..210 {
        post(
            &mut board,
            "claude",
            plan,
            EntryKind::Question,
            &format!("reminder {index}"),
            None,
        );
    }
    let inbox = scoped_feed(&mut board, None, true, 100);
    assert_eq!(inbox.open.len(), 20);
    assert_eq!(inbox.open[0].body.as_str(), "reminder 0");
    assert_eq!(
        inbox.open_omitted, 180,
        "the reminder count stops at its cap instead of scanning every entry"
    );
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
