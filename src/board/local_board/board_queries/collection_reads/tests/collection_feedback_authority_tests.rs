use super::*;

#[test]
fn collection_attention_excludes_feedback_and_feedback_page_keeps_active_reports() {
    let (_directory, board) = database();
    let repo = register_repositories(&board.conn);
    for (entry, plan, reporter, to) in [
        (3, 1, 2, None),
        (4, 1, 4, None),
        (5, 1, 3, None),
        (6, 1, 4, None),
        (7, 2, 2, None),
        (8, 2, 1, Some("muse")),
        (9, 1, 1, None),
        (10, 2, 2, Some("codex")),
    ] {
        seed_entry(
            &board.conn,
            entry,
            entry,
            plan,
            "feedback",
            reporter,
            to,
            "report",
        );
    }
    board
        .conn
        .execute_batch(concat!(
            "UPDATE entries SET state='open' WHERE kind='feedback'; ",
            "UPDATE entries SET state='triaged' WHERE id=3; UPDATE entries SET state='fixed' WHERE id=9; ",
            "UPDATE entries SET plan_id=NULL WHERE id IN (5,6); UPDATE plans SET owner_user='other' WHERE id=2;"
        ))
        .unwrap();
    board
        .conn
        .execute("UPDATE entries SET repo_key=?1 WHERE id=5", [repo.as_str()])
        .unwrap();
    for entry in 3..=10 {
        board
            .conn
            .execute(
                concat!(
                    "INSERT INTO board_feedback(entry_id,feedback_kind,version,build_id,cwd,steer_mode,",
                    "recent_calls_json) VALUES(?1,'missing','v1','build1','src','plan','[]')"
                ),
                [entry],
            )
            .unwrap();
    }
    grant_all_to_claude(&board.conn, plan(1));
    let reader = board.reader.as_ref().expect("read connection");
    assert_eq!(
        feedback(reader, true, None, None, 200)
            .feedback
            .iter()
            .map(|report| report.entry.id)
            .collect::<Vec<_>>(),
        vec![id(3), id(4), id(5), id(6), id(7), id(8), id(10)],
        "open and triaged feedback stays available through FeedbackList"
    );
    for actor in [
        context(),
        authority_actor("josh", "laptop", "human", "h1"),
        authority_actor("josh", "laptop", "claude", "s2"),
        authority_actor("other", "laptop", "human", "h1"),
        authority_actor("other", "laptop", "claude", "s2"),
    ] {
        assert!(
            attention_page(reader, &actor, None, true, 200)
                .entries
                .is_empty(),
            "feedback stays off Needs you regardless of authority"
        );
    }
}
