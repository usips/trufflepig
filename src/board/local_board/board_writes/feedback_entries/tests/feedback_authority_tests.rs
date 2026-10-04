use super::*;

#[test]
fn terminal_feedback_cannot_be_changed_to_another_terminal_state() {
    let (_dir, mut board) = board();
    let entry = changed(board.handle(&report("original")).unwrap()).entry;
    board
        .conn
        .execute(
            "UPDATE entries SET state='triaged' WHERE id=?1",
            [sqlite_id(entry.get()).unwrap()],
        )
        .unwrap();
    board
        .handle(&BoardRequest::new(
            human_actor("triage"),
            BoardOp::FeedbackClose {
                entry,
                state: FeedbackState::Wontfix,
                note: None,
            },
        ))
        .unwrap();
    let error = board
        .handle(&BoardRequest::new(
            human_actor("other"),
            BoardOp::FeedbackClose {
                entry,
                state: FeedbackState::Duplicate,
                note: None,
            },
        ))
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidState
    );
    assert_eq!(
        read_feedback(&board.conn, false).unwrap()[0].state,
        FeedbackState::Wontfix
    );
}

#[test]
fn feedback_triage_and_close_require_owner_authority() {
    let (_dir, mut board) = board();
    let entry = changed(board.handle(&report("original")).unwrap()).entry;
    for unauthorized in [
        actor("untrusted"),
        BoardActor::new(
            "other",
            "laptop",
            HarnessLabel::parse("human").unwrap(),
            "human",
        )
        .unwrap(),
    ] {
        let error = board
            .handle(&BoardRequest::new(
                unauthorized,
                BoardOp::FeedbackClose {
                    entry,
                    state: FeedbackState::Fixed,
                    note: None,
                },
            ))
            .unwrap_err();
        assert_eq!(
            error.code,
            crate::board::board_protocol::BoardErrorCode::InvalidActor
        );
    }
    board
        .handle(&BoardRequest::new(
            human_actor("triage"),
            BoardOp::FeedbackTriage {
                entry,
                note: Some(EntryText::new("confirmed reproduction").unwrap()),
            },
        ))
        .unwrap();
    assert_eq!(
        read_feedback(&board.conn, true).unwrap()[0].state,
        FeedbackState::Triaged
    );
    board
        .handle(&BoardRequest::new(
            human_actor("close"),
            BoardOp::FeedbackClose {
                entry,
                state: FeedbackState::Fixed,
                note: None,
            },
        ))
        .unwrap();
    assert!(read_feedback(&board.conn, true).unwrap().is_empty());
}

#[test]
fn plan_feedback_requires_the_owners_human_or_steward() {
    let (_dir, mut board) = board();
    let plan = changed(
        board
            .handle(&BoardRequest::new(
                actor("owner"),
                BoardOp::New {
                    title: crate::board::board_vocabulary::PlanTitle::new("Feedback scope")
                        .unwrap(),
                    body: crate::board::board_vocabulary::PlanText::new("").unwrap(),
                    steward: Some(HarnessLabel::parse("claude").unwrap()),
                    repo_key: None,
                },
            ))
            .unwrap(),
    )
    .plan
    .unwrap();
    let mut request = report("reporter");
    let BoardOp::Feedback {
        plan: report_plan, ..
    } = &mut request.op
    else {
        unreachable!()
    };
    *report_plan = Some(plan);
    let entry = changed(board.handle(&request).unwrap()).entry;
    let other_user = BoardActor::new(
        "other",
        "laptop",
        HarnessLabel::parse("claude").unwrap(),
        "steward",
    )
    .unwrap();
    assert!(
        board
            .handle(&BoardRequest::new(
                other_user,
                BoardOp::FeedbackTriage { entry, note: None }
            ))
            .is_err()
    );
    let steward = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("claude").unwrap(),
        "steward",
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            steward,
            BoardOp::FeedbackTriage { entry, note: None },
        ))
        .unwrap();
}
