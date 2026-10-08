use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn collection_dispatch_reports_snapshot_without_registering_reader_actor() {
    let (mut board, path) = database();
    let plan = new_plan(&mut board, actor("human", "owner"), "Read collections")
        .plan
        .unwrap();
    let actor_count: i64 = board
        .conn
        .query_row("SELECT count(*) FROM actors", [], |row| row.get(0))
        .unwrap();
    let session_count: i64 = board
        .conn
        .query_row("SELECT count(*) FROM agent_sessions", [], |row| row.get(0))
        .unwrap();
    let latest = board.max_seq().unwrap();
    let observer = actor("codex", "unregistered-reader");
    let operations = [
        BoardOp::Overview {
            scope: ReadScope::All,
            after: None,
            through: None,
            limit: 10,
        },
        BoardOp::Attention {
            scope: ReadScope::All,
            after: None,
            through: None,
            limit: 10,
        },
        BoardOp::Feed {
            scope: ReadScope::All,
            plan: Some(plan),
            after: None,
            through: None,
            limit: 10,
        },
        BoardOp::History {
            plan,
            after: None,
            through: None,
            limit: 10,
        },
        BoardOp::Entries {
            plan: Some(plan),
            kind: None,
            harness: None,
            user: None,
            host: None,
            task: None,
            references: None,
            after: None,
            before: None,
            through: None,
            limit: 10,
        },
        BoardOp::Tasks {
            column: None,
            order: TaskOrder::Ordinal,
            before: None,
            plan,
            after: None,
            ceiling: None,
            through: None,
            limit: 10,
        },
        BoardOp::Claims {
            scope: ReadScope::All,
            plan: Some(plan),
            own_stale: false,
            after: None,
            through: None,
            limit: 10,
        },
        BoardOp::FeedbackList {
            open_only: false,
            after: None,
            through: None,
            limit: 10,
        },
    ];
    let writer_changes = board.conn.total_changes();
    for op in operations {
        assert!(op.is_read_only());
        let reply = board
            .handle(&BoardRequest::new(observer.clone(), op))
            .unwrap();
        assert_eq!(reply.snapshot_seq, Some(latest));
    }
    assert_eq!(board.conn.total_changes(), writer_changes);
    assert_eq!(board.reader.as_ref().unwrap().total_changes(), 0);
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM actors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        actor_count
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM agent_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        session_count
    );
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
