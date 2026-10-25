use super::*;
use crate::board::board_protocol::AuthorityRole;

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
fn only_the_exact_reporter_gets_the_reporter_close_path() {
    let (_dir, mut board) = board();
    let entry = changed(board.handle(&report("original")).unwrap()).entry;
    let error = board
        .handle(&BoardRequest::new(
            actor("sibling"),
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
    board
        .handle(&BoardRequest::new(
            actor("original"),
            BoardOp::FeedbackClose {
                entry,
                state: FeedbackState::Fixed,
                note: None,
            },
        ))
        .unwrap();
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
fn plan_feedback_requires_owner_or_explicit_triage_grant() {
    let (_dir, mut board) = board();
    let plan = changed(
        board
            .handle(&BoardRequest::new(
                actor("owner"),
                BoardOp::New {
                    title: crate::board::board_vocabulary::PlanTitle::new("Feedback scope")
                        .unwrap(),
                    body: crate::board::board_vocabulary::PlanText::new("").unwrap(),
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
    grant(
        &mut board,
        human_actor("grantor"),
        Some(plan),
        vec![AuthorityRole::Triage],
        "claude",
        None,
    );
    let other_user = BoardActor::new(
        "other",
        "laptop",
        HarnessLabel::parse("claude").unwrap(),
        "session",
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
    let claude_session = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("claude").unwrap(),
        "session",
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            claude_session,
            BoardOp::FeedbackTriage { entry, note: None },
        ))
        .unwrap();
}

#[test]
fn harness_session_grants_normalize_to_the_owner_identity() {
    let (_dir, mut board) = board();
    let plan = changed(
        board
            .handle(&BoardRequest::new(
                actor("owner"),
                BoardOp::New {
                    title: crate::board::board_vocabulary::PlanTitle::new("Recipient scope")
                        .unwrap(),
                    body: crate::board::board_vocabulary::PlanText::new("").unwrap(),
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
    let grant = grant(
        &mut board,
        human_actor("grantor"),
        Some(plan),
        vec![AuthorityRole::Triage],
        "claude/reviewer",
        None,
    );
    let grantee: String = board
        .conn
        .query_row(
            "SELECT grantee FROM authority_grants WHERE entry_id=?1",
            [sqlite_id(grant.get()).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(grantee, "josh@laptop/claude/reviewer");

    let exact = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("claude").unwrap(),
        "reviewer",
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            exact,
            BoardOp::FeedbackTriage { entry, note: None },
        ))
        .unwrap();

    let mut sibling_request = report("second-reporter");
    let BoardOp::Feedback {
        plan: sibling_plan, ..
    } = &mut sibling_request.op
    else {
        unreachable!()
    };
    *sibling_plan = Some(plan);
    let sibling_report = changed(board.handle(&sibling_request).unwrap()).entry;
    let sibling = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("claude").unwrap(),
        "another-session",
    )
    .unwrap();
    assert!(
        board
            .handle(&BoardRequest::new(
                sibling,
                BoardOp::FeedbackTriage {
                    entry: sibling_report,
                    note: None,
                },
            ))
            .is_err()
    );
}

#[test]
fn board_triage_grants_cover_only_planless_reports_from_the_grant_owner() {
    let (_dir, mut board) = board();
    let owner_report = changed(board.handle(&report("owner-report")).unwrap()).entry;
    let mut foreign_request = report("foreign-report");
    let BoardOp::Feedback { summary, .. } = &mut foreign_request.op else {
        unreachable!()
    };
    *summary = EntryText::new("foreign report result was incorrect").unwrap();
    foreign_request.actor = BoardActor::new(
        "other",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "foreign-report",
    )
    .unwrap();
    let foreign_report = changed(board.handle(&foreign_request).unwrap()).entry;
    let grant_entry = grant(
        &mut board,
        human_actor("grantor"),
        None,
        vec![AuthorityRole::Triage],
        "muse",
        None,
    );
    let grantee = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("muse").unwrap(),
        "triager",
    )
    .unwrap();
    let triaged = board
        .handle(&BoardRequest::new(
            grantee.clone(),
            BoardOp::FeedbackTriage {
                entry: owner_report,
                note: None,
            },
        ))
        .unwrap();
    board
        .handle(&BoardRequest::new(
            human_actor("grantor"),
            BoardOp::Revoke { entry: grant_entry },
        ))
        .unwrap();
    let replay = board
        .handle(&BoardRequest::new(
            grantee.clone(),
            BoardOp::FeedbackTriage {
                entry: owner_report,
                note: None,
            },
        ))
        .unwrap();
    let (
        crate::board::board_protocol::BoardResult::Change(triaged_change),
        crate::board::board_protocol::BoardResult::Change(replay_change),
    ) = (triaged.result, replay.result)
    else {
        panic!("expected triage change receipts");
    };
    assert!(!triaged_change.deduplicated);
    assert!(replay_change.deduplicated);
    assert_eq!(replay_change.entry, triaged_change.entry);
    assert_eq!(
        board
            .handle(&BoardRequest::new(
                grantee.clone(),
                BoardOp::FeedbackTriage {
                    entry: owner_report,
                    note: Some(EntryText::new("new mutation").unwrap()),
                },
            ))
            .unwrap_err()
            .code,
        crate::board::board_protocol::BoardErrorCode::InvalidActor
    );
    assert_eq!(
        board
            .handle(&BoardRequest::new(
                grantee,
                BoardOp::FeedbackTriage {
                    entry: foreign_report,
                    note: None,
                },
            ))
            .unwrap_err()
            .code,
        crate::board::board_protocol::BoardErrorCode::InvalidActor
    );
}

#[test]
fn triage_grants_enforce_role_expiry_and_revocation() {
    let (_dir, mut board) = board();
    let plan = changed(
        board
            .handle(&BoardRequest::new(
                actor("owner"),
                BoardOp::New {
                    title: crate::board::board_vocabulary::PlanTitle::new("Grant scope").unwrap(),
                    body: crate::board::board_vocabulary::PlanText::new("").unwrap(),
                    repo_key: None,
                },
            ))
            .unwrap(),
    )
    .plan
    .unwrap();
    let mut report_request = report("original");
    let BoardOp::Feedback {
        plan: report_plan, ..
    } = &mut report_request.op
    else {
        unreachable!()
    };
    *report_plan = Some(plan);
    let entry = changed(board.handle(&report_request).unwrap()).entry;

    grant(
        &mut board,
        human_actor("grantor"),
        Some(plan),
        vec![AuthorityRole::Link],
        "muse",
        None,
    );
    let link_only = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("muse").unwrap(),
        "session",
    )
    .unwrap();
    assert!(
        board
            .handle(&BoardRequest::new(
                link_only,
                BoardOp::FeedbackTriage { entry, note: None },
            ))
            .is_err()
    );

    let expiring = grant(
        &mut board,
        human_actor("grantor"),
        Some(plan),
        vec![AuthorityRole::Triage],
        "claude",
        Some(60),
    );
    board
        .conn
        .execute(
            "UPDATE authority_grants SET expires_at=0 WHERE entry_id=?1",
            [sqlite_id(expiring.get()).unwrap()],
        )
        .unwrap();
    let expired = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("claude").unwrap(),
        "session",
    )
    .unwrap();
    assert!(
        board
            .handle(&BoardRequest::new(
                expired,
                BoardOp::FeedbackTriage { entry, note: None },
            ))
            .is_err()
    );

    let revocable = grant(
        &mut board,
        human_actor("grantor"),
        Some(plan),
        vec![AuthorityRole::Triage],
        "kimi",
        None,
    );
    board
        .handle(&BoardRequest::new(
            human_actor("grantor"),
            BoardOp::Revoke { entry: revocable },
        ))
        .unwrap();
    let revoked = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("kimi").unwrap(),
        "session",
    )
    .unwrap();
    assert!(
        board
            .handle(&BoardRequest::new(
                revoked,
                BoardOp::FeedbackTriage { entry, note: None },
            ))
            .is_err()
    );
    assert_eq!(read_feedback(&board.conn, true).unwrap()[0].entry.id, entry);
}

fn grant(
    board: &mut LocalBoard,
    actor: BoardActor,
    plan: Option<crate::board::board_ids::PlanId>,
    roles: Vec<AuthorityRole>,
    grantee: &str,
    expires_in: Option<u64>,
) -> EntryId {
    changed(
        board
            .handle(&BoardRequest::new(
                actor,
                BoardOp::Grant {
                    plan,
                    roles,
                    grantee: BoardRecipient::parse(grantee).unwrap(),
                    expires_in,
                    note: None,
                },
            ))
            .unwrap(),
    )
    .entry
}
