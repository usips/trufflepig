use super::*;

#[test]
fn board_search_uses_a_query_only_committed_snapshot_without_actor_writes() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board, "One", "committedneedle body");
    let snapshot = board.max_seq().unwrap();
    board.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    board.conn.execute(
        "INSERT INTO entries(plan_id,kind,body,actor_id,seq,created_at) VALUES(?1,'note','draftneedle body',1,2,1)",
        [sql_number(plan.get())],
    ).unwrap();
    let before = board.conn.total_changes();
    let newcomer = BoardActor::new(
        "new-user",
        "host",
        HarnessLabel::parse("codex").unwrap(),
        "never-written",
    )
    .unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            newcomer.clone(),
            query_op("committedneedle", Some(plan), 20),
        ))
        .unwrap();
    assert_eq!(reply.snapshot_seq, Some(snapshot));
    let visible = serde_json::to_value(reply.result).unwrap();
    assert_eq!(targets(&visible["data"]), ["P1@1".to_owned()].into());
    let draft = board
        .handle(&BoardRequest::new(
            newcomer,
            query_op("draftneedle", None, 20),
        ))
        .unwrap();
    let visible = serde_json::to_value(draft.result).unwrap();
    assert!(targets(&visible["data"]).is_empty());
    assert_eq!(draft.snapshot_seq, Some(snapshot));
    assert_eq!(board.conn.total_changes(), before);
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM actors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        board
            .reader
            .as_ref()
            .unwrap()
            .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    board.conn.execute_batch("ROLLBACK").unwrap();
    integrity(&board);
}
