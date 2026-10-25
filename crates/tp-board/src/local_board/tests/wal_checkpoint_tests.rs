use super::*;
use std::time::Instant;

#[test]
fn writer_bounds_its_wal_and_truncates_it_without_waiting_on_readers() {
    let directory = tp_core::test_fixtures::scratch("board-wal-");
    let path = directory.path().join("board.sqlite3");
    let wal = directory.path().join("board.sqlite3-wal");
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let limit: i64 = board
        .conn
        .query_row("PRAGMA journal_size_limit", [], |row| row.get(0))
        .unwrap();
    assert_eq!(limit, board_database::BOARD_JOURNAL_SIZE_LIMIT);

    new_plan(&mut board, actor("codex", "writer"), "Before the snapshot");
    let reader = Connection::open(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let plans: i64 = reader
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    assert_eq!(plans, 1);
    new_plan(&mut board, actor("codex", "writer"), "After the snapshot");
    assert!(wal.metadata().unwrap().len() > 0);

    // The open snapshot keeps the WAL, and the checkpoint does not wait on
    // the writer's 5 s busy handler for it.
    let started = Instant::now();
    assert!(!board.checkpoint_wal().unwrap());
    assert!(started.elapsed() < Duration::from_secs(2));
    let still: i64 = reader
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    assert_eq!(still, 1, "the reader kept its snapshot");
    reader.execute_batch("COMMIT").unwrap();

    assert!(board.checkpoint_wal().unwrap());
    assert_eq!(wal.metadata().unwrap().len(), 0);
    new_plan(&mut board, actor("codex", "writer"), "After the checkpoint");
    let read_only = LocalBoard::open_read_with_timeout(
        &crate::BoardConfig::for_database(&path),
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(!read_only.checkpoint_wal().unwrap());
}
