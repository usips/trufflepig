use super::*;

#[test]
fn collection_feedback_pages_filter_states_and_keep_frozen_shared_sequences() {
    let (_directory, mut board) = database();
    let repo = register_repositories(&board.conn);
    seed_feedback(&board.conn, 3, 3, "open");
    seed_feedback(&board.conn, 4, 3, "triaged");
    seed_feedback(&board.conn, 5, 4, "fixed");
    seed_feedback(&board.conn, 6, 5, "open");
    board
        .conn
        .execute(
            "UPDATE entries SET plan_id=NULL,repo_key=?1 WHERE id=3",
            [repo.as_str()],
        )
        .unwrap();
    let tx = board
        .reader
        .as_mut()
        .expect("read connection")
        .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
        .unwrap();
    let first = feedback(&tx, true, None, None, 1);
    assert!(first.open_only);
    assert_eq!(first.feedback[0].entry.id, id(3));
    assert_eq!(first.feedback[0].entry.plan, None);
    assert_eq!(first.feedback[0].metadata.repo_key, Some(repo));
    assert_eq!(first.feedback[0].metadata.version, "v1");
    assert_eq!(
        first.feedback[0].metadata.build_id.as_deref(),
        Some("build1")
    );
    assert_eq!(first.feedback[0].metadata.cwd, "src");
    assert_eq!(
        first.feedback[0].metadata.steer_mode.as_deref(),
        Some("plan")
    );
    assert_eq!(
        first.feedback[0].metadata.recent_calls[0].args,
        vec!["needle"]
    );
    assert_eq!(first.feedback[0].kind, FeedbackKind::Missing);
    assert_eq!(
        first.feedback[0].state,
        crate::board::board_vocabulary::FeedbackState::Open
    );
    assert_eq!(first.omitted, 2);
    assert_eq!(first.through, EventSeq::new(5));
    seed_feedback(&board.conn, 7, 6, "open");
    let second = feedback(&tx, true, first.next_after, Some(first.through), 1);
    assert_eq!(second.feedback[0].entry.id, id(4));
    assert_eq!(second.omitted, 1);
    let third = feedback(&tx, true, second.next_after, Some(first.through), 1);
    assert_eq!(third.feedback[0].entry.id, id(6));
    assert_eq!(third.omitted, 0);
    assert_eq!(third.next_after, None);
    let all = feedback(&tx, false, None, Some(first.through), 200);
    assert!(!all.open_only);
    assert_eq!(
        all.feedback
            .iter()
            .map(|report| report.entry.id)
            .collect::<Vec<_>>(),
        vec![id(3), id(4), id(5), id(6)]
    );
    tx.commit().unwrap();
    let frozen = feedback(
        board.reader.as_ref().expect("read connection"),
        true,
        None,
        Some(first.through),
        200,
    );
    assert_eq!(frozen.feedback.len(), 3);
    assert!(
        !frozen
            .feedback
            .iter()
            .any(|report| report.entry.id == id(7))
    );
    assert_eq!(
        feedback_page(
            board.reader.as_ref().expect("read connection"),
            true,
            None,
            None,
            201
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        feedback_page(
            board.reader.as_ref().expect("read connection"),
            true,
            Some(EntryCursor {
                seq: EventSeq::new(6),
                entry: id(7)
            }),
            Some(first.through),
            1
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
    board
        .conn
        .execute(
            "UPDATE board_feedback SET recent_calls_json='invalid' WHERE entry_id=7",
            [],
        )
        .unwrap();
    assert_eq!(
        feedback_page(
            board.reader.as_ref().expect("read connection"),
            true,
            None,
            None,
            200
        )
        .unwrap_err()
        .code,
        BoardErrorCode::BoardUnavailable
    );
}
