use super::*;

#[test]
fn reminder_count_query_drives_from_the_kind_index() {
    let (_directory, board) = database();
    board
        .conn
        .execute_batch(
            "INSERT INTO actors VALUES(1,'josh','laptop','codex','s1');
             INSERT INTO plans(id,title,owner_user,head_revision,created_at)
             VALUES(1,'One','josh',1,1);
             INSERT INTO texts VALUES('one','body');
             INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
             VALUES(0,1,'create','One',1,0,50);
             INSERT INTO revisions VALUES(1,1,'one','create',0,1,0);",
        )
        .unwrap();
    for number in 1..=300u64 {
        let kind = match number % 10 {
            0 => "question",
            1 => "proposal",
            2 => "feedback",
            _ => "note",
        };
        board
            .conn
            .execute(
                "INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at) \
                 VALUES(?1,1,?2,'body',1,?3,50)",
                params![number as i64, kind, number as i64],
            )
            .unwrap();
    }
    board.conn.execute_batch("ANALYZE;").unwrap();
    let predicate = crate::board::local_board::board_queries::board_reads::entry_reference_reads::reminder_predicate();
    let plan: Vec<String> = board
        .conn
        .prepare(&format!(
            "EXPLAIN QUERY PLAN SELECT count(*) FROM (SELECT 1 FROM entries e WHERE {predicate} LIMIT ?7)"
        ))
        .unwrap()
        .query_map(
            params![
                "josh",
                "codex",
                "josh@laptop/codex/s1",
                false,
                None::<&str>,
                i64::MAX,
                200i64
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|detail| detail.starts_with("SEARCH e USING INDEX entries_kind_state")),
        "reminders must drive the entries scan from entries_kind_state: {plan:?}"
    );
    assert!(
        !plan.iter().any(|detail| detail.starts_with("SCAN e")),
        "reminders must not scan the whole entries table: {plan:?}"
    );
}
