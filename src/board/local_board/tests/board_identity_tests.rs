use super::*;

#[test]
fn local_board_uuid_is_stable_across_reopens() {
    let (board, path) = database();
    let first = board.board_uuid().unwrap();
    assert_eq!(
        uuid::Uuid::parse_str(&first).unwrap().get_version(),
        Some(uuid::Version::Random)
    );
    drop(board);
    let reopened = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    assert_eq!(reopened.board_uuid().unwrap(), first);
    drop(reopened);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn local_board_uuid_backfills_when_missing() {
    let (board, path) = database();
    let first = board.board_uuid().unwrap();
    drop(board);
    let conn = Connection::open(&path).unwrap();
    conn.execute("DELETE FROM board_meta WHERE key='board_uuid'", [])
        .unwrap();
    drop(conn);
    let reopened = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let second = reopened.board_uuid().unwrap();
    assert!(uuid::Uuid::parse_str(&second).is_ok());
    assert_ne!(second, first);
    drop(reopened);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
