use super::*;

#[test]
fn local_board_refuses_newer_schema_without_changing_journal() {
    let (mut board, path) = database();
    drop(board.reader.take());
    board.conn.pragma_update(None, "user_version", 99).unwrap();
    board
        .conn
        .pragma_update(None, "journal_mode", "DELETE")
        .unwrap();
    drop(board);
    let error = match LocalBoard::open_path(&path, Duration::from_secs(120)) {
        Err(error) => error,
        Ok(_) => panic!("accepted newer schema"),
    };
    assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
    let conn = Connection::open(&path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    drop(conn);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn local_board_concurrent_first_open_serializes_migration() {
    let path = crate::board::board_test_support::scratch("board-storage-")
        .keep()
        .join("board.sqlite3");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|index| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
                new_plan(
                    &mut board,
                    actor("human", &format!("h{index}")),
                    &format!("Plan {index}"),
                )
                .plan
                .unwrap()
            })
        })
        .collect();
    let ids: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_ne!(ids[0], ids[1]);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn local_board_lazy_open_respects_the_supplied_lock_timeout() {
    let (board, path) = database();
    board.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let error = match LocalBoard::open_path_with_timeout(
        &path,
        Duration::from_secs(120),
        Duration::from_millis(20),
    ) {
        Err(error) => error,
        Ok(_) => panic!("opened a locked database"),
    };
    assert_eq!(error.code, BoardErrorCode::DatabaseLocked);
    assert!(started.elapsed() < Duration::from_millis(500));
    board.conn.execute_batch("ROLLBACK").unwrap();
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn local_board_preserves_readonly_directory_and_private_file_modes() {
    use std::os::unix::fs::PermissionsExt;
    let (mut board, path) = database();
    let created = new_plan(&mut board, actor("human", "h1"), "Trial");
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    let parent = path.parent().unwrap();
    assert_eq!(
        parent.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failure = board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::Post {
                target: BoardRef::Plan(created.plan.unwrap()),
                kind: EntryKind::Note,
                body: EntryText::new("blocked write").unwrap(),
                to: None,
                supersedes: None,
            },
        ))
        .unwrap_err();
    assert_eq!(failure.code, BoardErrorCode::BoardUnavailable);
    assert!(LocalBoard::open_path(&path, Duration::from_secs(120)).is_err());
    assert_eq!(
        parent.metadata().unwrap().permissions().mode() & 0o777,
        0o500
    );
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(board);
    std::fs::remove_dir_all(parent).unwrap();
}
