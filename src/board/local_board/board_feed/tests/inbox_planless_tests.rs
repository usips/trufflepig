use super::*;

#[test]
fn planless_events_stay_out_of_scoped_inboxes_but_own_feedback_outcomes_arrive() {
    use crate::board::board_protocol::FeedbackMetadata;
    use crate::board::board_vocabulary::{FeedbackKind, FeedbackState};
    let (_directory, mut board) = database();
    let plan_a = plan(&mut board);
    let BoardResult::Change(created_b) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Second repo plan").unwrap(),
            body: PlanText::new("# Scope").unwrap(),
            steward: None,
            repo_key: None,
        },
    ) else {
        panic!("missing plan");
    };
    let plan_b = created_b.plan.unwrap();
    let repo_a = RepoKey::from_roots([crate::identity::GitOid::parse(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .unwrap()])
    .unwrap();
    let repo_b = RepoKey::from_roots([crate::identity::GitOid::parse(
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )
    .unwrap()])
    .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO repos(repo_key) VALUES(?1),(?2)",
            [repo_a.as_str(), repo_b.as_str()],
        )
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO plan_repos(plan_id,repo_key) VALUES(?1,?2),(?3,?4)",
            params![
                sql_number(plan_a.get()),
                repo_a.as_str(),
                sql_number(plan_b.get()),
                repo_b.as_str()
            ],
        )
        .unwrap();
    call(
        &mut board,
        "codex",
        BoardOp::Hello {
            model: "Codex".into(),
            effort: None,
        },
    );
    call(
        &mut board,
        "muse",
        BoardOp::Hello {
            model: "Muse".into(),
            effort: None,
        },
    );
    post(
        &mut board,
        "muse",
        plan_b,
        EntryKind::Note,
        "repo b news",
        None,
    );
    call(
        &mut board,
        "muse",
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("session b stray report").unwrap(),
            body: None,
            plan: None,
            metadata: FeedbackMetadata::default(),
            import_key: None,
        },
    );
    let scoped_a = scoped_feed(&mut board, Some(repo_a.clone()), false, 100);
    assert!(
        !scoped_a
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Hello),
        "a planless hello broadcast is not scoped inbox news"
    );
    assert!(
        !scoped_a
            .events
            .iter()
            .any(|event| event.summary.as_str() == "session b stray report"),
        "another session's repo-less feedback report stays out of this scope"
    );
    assert!(
        !scoped_a
            .events
            .iter()
            .any(|event| event.summary.as_str() == "repo b news")
    );
    let BoardResult::Inbox(scoped_b) = call(
        &mut board,
        "muse",
        BoardOp::Inbox {
            after: Some(EventSeq::new(0)),
            limit: 100,
            repo_key: Some(repo_b),
            all: false,
        },
    ) else {
        panic!("missing inbox");
    };
    assert!(
        !scoped_b
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Hello),
        "no hello event reaches any scoped inbox"
    );
    let BoardResult::Change(own) = call(
        &mut board,
        "codex",
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("my own stray report").unwrap(),
            body: None,
            plan: None,
            metadata: FeedbackMetadata::default(),
            import_key: None,
        },
    ) else {
        panic!("missing report");
    };
    call(
        &mut board,
        "human",
        BoardOp::FeedbackClose {
            entry: own.entry,
            state: FeedbackState::Fixed,
            note: None,
        },
    );
    let scoped_a = scoped_feed(&mut board, Some(repo_a.clone()), false, 100);
    assert!(
        scoped_a
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Feedback
                && event.subject == BoardRef::Entry(own.entry)),
        "the outcome of the caller's own repo-less feedback still arrives"
    );
    assert!(
        !scoped_a
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Hello)
    );
    let global = scoped_feed(&mut board, Some(repo_a), true, 100);
    assert!(
        global
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Feedback
                && event.subject == BoardRef::Entry(own.entry)),
        "the global inbox keeps planless addressed and own-feedback events"
    );
}

#[test]
fn hello_events_stay_out_of_every_inbox_including_all() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Note,
        "foreign news",
        None,
    );
    call(
        &mut board,
        "muse",
        BoardOp::Hello {
            model: "Muse".into(),
            effort: None,
        },
    );
    let global = scoped_feed(&mut board, None, true, 100);
    assert!(
        global
            .events
            .iter()
            .any(|event| event.summary.as_str() == "foreign news")
    );
    assert!(
        !global
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Hello),
        "a hello broadcast is session state, not inbox news, even with --all"
    );
}

#[test]
fn feedback_outcomes_reach_new_sessions_of_the_reporting_harness() {
    use crate::board::board_protocol::FeedbackMetadata;
    use crate::board::board_vocabulary::{FeedbackKind, FeedbackState};
    let (_directory, mut board) = database();
    let BoardResult::Change(report) = call(
        &mut board,
        "codex",
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("stray report").unwrap(),
            body: None,
            plan: None,
            metadata: FeedbackMetadata::default(),
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
    let inbox = |board: &mut LocalBoard, harness: &str, session: &str| {
        let actor = BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse(harness).unwrap(),
            session,
        )
        .unwrap();
        match board
            .handle(&BoardRequest::new(
                actor,
                BoardOp::Inbox {
                    after: Some(EventSeq::new(0)),
                    limit: 100,
                    repo_key: None,
                    all: false,
                },
            ))
            .unwrap()
            .result
        {
            BoardResult::Inbox(inbox) => inbox,
            other => panic!("unexpected {other:?}"),
        }
    };
    let outcome = |event: &EventRecord| {
        event.kind == EntryKind::Feedback
            && event.subject == BoardRef::Entry(report.entry)
            && event.summary.as_str().starts_with("closed")
    };
    assert!(
        inbox(&mut board, "codex", "session2")
            .events
            .iter()
            .any(outcome),
        "a new session of the reporting harness keeps the outcome"
    );
    assert!(
        !inbox(&mut board, "muse", "session1")
            .events
            .iter()
            .any(outcome),
        "another harness still omits the feedback outcome"
    );
}
