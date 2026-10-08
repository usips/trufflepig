use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn show_uses_read_connection_while_writer_transaction_is_held() {
    let (mut board, path) = database();
    let plan = new_plan(&mut board, actor("human", "h1"), "Read during write")
        .plan
        .unwrap();
    board.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "new-reader"),
            BoardOp::Show {
                target: BoardRef::Plan(plan),
            },
        ))
        .unwrap();
    board.conn.execute_batch("ROLLBACK").unwrap();
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn readonly_dispatch_leaves_actors_sessions_and_claims_untouched() {
    let (mut board, path) = database();
    let created = new_plan(&mut board, actor("human", "h1"), "Read evidence");
    let plan = created.plan.unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new("Lane").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::ClaimTask {
                task: crate::board::board_ids::TaskId::new(plan, 1).unwrap(),
                scope: Some(EntryText::new("scope").unwrap()),
                resume: ClaimResume::No,
                delegate: None,
            },
        ))
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE agent_sessions SET last_seen=7,model='stored-model',effort='stored-effort'",
            [],
        )
        .unwrap();
    board
        .conn
        .execute("UPDATE claims SET last_active=9", [])
        .unwrap();
    let before = board.conn.total_changes();
    board.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut request = BoardRequest::new(
        actor("codex", "c1"),
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
    );
    request.claims = Some(AgentClaims {
        model: Some("replacement".into()),
        effort: Some("replacement".into()),
    });
    board.handle(&request).unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::Inbox {
                scope: ReadScope::All,
                after: Some(EventSeq::new(0)),
                limit: 20,
            },
        ))
        .unwrap();
    board
        .handle(&BoardRequest::new(
            actor("new-harness", "never-written"),
            BoardOp::Overview {
                scope: ReadScope::All,
                after: None,
                through: None,
                limit: 200,
            },
        ))
        .unwrap();
    assert_eq!(board.conn.total_changes(), before);
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM actors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        board
            .conn
            .query_row(
                concat!(
                    "SELECT count(*) FROM agent_sessions WHERE last_seen<>7 ",
                    "OR model<>'stored-model' OR effort<>'stored-effort'"
                ),
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT last_active FROM claims", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        board
            .reader
            .as_ref()
            .unwrap()
            .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
            .unwrap(),
        true
    );
    board.conn.execute_batch("ROLLBACK").unwrap();
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn readonly_open_answers_all_read_kinds_without_write_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (mut writer, path) = database();
    let plan = new_plan(&mut writer, actor("human", "h1"), "Readonly evidence")
        .plan
        .unwrap();
    writer
        .conn
        .execute("UPDATE agent_sessions SET last_seen=17", [])
        .unwrap();
    drop(writer);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    std::fs::set_permissions(
        path.parent().unwrap(),
        std::fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let config = BoardConfig::for_database(&path);
    let mut reader =
        LocalBoard::open_read_with_timeout(&config, Duration::from_millis(100)).unwrap();
    for op in [
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
        BoardOp::Review {
            base: crate::board::board_ids::PlanRevision::new(plan, 1).unwrap(),
            agent: None,
        },
        BoardOp::FeedbackList {
            open_only: false,
            after: None,
            through: None,
            limit: 200,
        },
    ] {
        reader
            .handle(&BoardRequest::new(actor("codex", "unseen-reader"), op))
            .unwrap();
    }
    assert_eq!(reader.conn.total_changes(), 0);
    assert!(
        reader.reader.is_none(),
        "readonly handles keep exactly one connection"
    );
    assert!(
        reader
            .conn
            .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
            .unwrap()
    );
    let error = reader
        .handle(&BoardRequest::new(
            actor("codex", "unseen-reader"),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new("forbidden write").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(reader.conn.total_changes(), 0);
    assert_eq!(
        reader
            .conn
            .query_row("SELECT count(*) FROM actors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        reader
            .conn
            .query_row("SELECT last_seen FROM agent_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert!(LocalBoard::open(&config).is_err());
    drop(reader);
    std::fs::set_permissions(
        path.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
