use super::*;

#[test]
fn board_search_indexes_plan_edits_proposals_and_posts_in_their_write_transaction() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board, "One", "initialneedle body");
    call(
        &mut board,
        BoardOp::Edit {
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("editedneedle body").unwrap(),
            summary: EntryText::new("edit summary").unwrap(),
        },
    );
    let proposal = call(
        &mut board,
        BoardOp::Propose {
            base: PlanRevision::new(plan, 2).unwrap(),
            body: PlanText::new("proposedneedle body").unwrap(),
            summary: EntryText::new("summaryneedle proposal").unwrap(),
            supersedes: None,
        },
    );
    let post = post(&mut board, plan, "postedneedle body");
    assert_eq!(
        targets(&results(&mut board, "initialneedle", None, 50)),
        ["P1@1".to_owned()].into()
    );
    assert_eq!(
        targets(&results(&mut board, "editedneedle", None, 50)),
        ["P1@2".to_owned()].into()
    );
    assert_eq!(
        targets(&results(&mut board, "proposedneedle", None, 50)),
        [proposal.entry.to_string()].into()
    );
    assert_eq!(
        targets(&results(&mut board, "summaryneedle", None, 50)),
        [proposal.entry.to_string()].into()
    );
    assert_eq!(
        targets(&results(&mut board, "postedneedle", None, 50)),
        [post.to_string()].into()
    );
    let before: i64 = board
        .conn
        .query_row("SELECT count(*) FROM search_documents", [], |row| {
            row.get(0)
        })
        .unwrap();
    board.conn.execute_batch("CREATE TRIGGER fail_search_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'injected event failure'); END;").unwrap();
    let error = board
        .handle(&BoardRequest::new(
            owner(),
            BoardOp::Edit {
                base: PlanRevision::new(plan, 2).unwrap(),
                body: PlanText::new("rollbackneedle body").unwrap(),
                summary: EntryText::new("rollback edit").unwrap(),
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
    board
        .conn
        .execute_batch("DROP TRIGGER fail_search_event;")
        .unwrap();
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM search_documents", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert!(targets(&results(&mut board, "rollbackneedle", None, 50)).is_empty());
    integrity(&board);
}

#[test]
fn board_search_external_content_update_delete_triggers_keep_integer_rowids_stable() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board, "One", "body");
    let entry = post(&mut board, plan, "beforeneedle body");
    let rowid: i64 = board
        .conn
        .query_row(
            "SELECT rowid FROM search_documents WHERE target=?1",
            [entry.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE entries SET body='afterneedle body' WHERE id=?1",
            [sql_number(entry.get())],
        )
        .unwrap();
    assert!(targets(&results(&mut board, "beforeneedle", None, 50)).is_empty());
    assert_eq!(
        targets(&results(&mut board, "afterneedle", None, 50)),
        [entry.to_string()].into()
    );
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT rowid FROM search_documents WHERE target=?1",
                [entry.to_string()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        rowid
    );
    board
        .conn
        .execute("DELETE FROM entries WHERE id=?1", [sql_number(entry.get())])
        .unwrap();
    assert!(targets(&results(&mut board, "afterneedle", None, 50)).is_empty());
    integrity(&board);
}
