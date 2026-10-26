use super::*;
use rusqlite::Connection;
use std::os::unix::net::UnixStream;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn board(with_reader: bool) -> LocalBoard {
    LocalBoard {
        conn: Connection::open_in_memory().unwrap(),
        reader: with_reader.then(|| Connection::open_in_memory().unwrap()),
        path: PathBuf::new(),
        claim_ttl_secs: 60,
        panic_after_write: false,
    }
}

fn recursive_sum(board: &LocalBoard, limit: i64) -> Result<i64, BoardError> {
    progress_connection(board)
        .query_row(
            concat!(
                "WITH RECURSIVE sequence(value) AS (",
                "VALUES(0) UNION ALL ",
                "SELECT value + 1 FROM sequence WHERE value < ?1",
                ") SELECT sum(value) FROM sequence"
            ),
            [limit],
            |row| row.get(0),
        )
        .map_err(sql_error)
}

#[test]
fn expired_recursive_read_is_interrupted_and_connection_is_reusable() {
    let mut board = board(false);
    let deadline = QueryDeadline::after(Duration::from_millis(5));
    let started = Instant::now();
    let error = with_query_deadline(&mut board, &deadline, |board| {
        recursive_sum(board, 50_000_000)
    })
    .unwrap_err();

    assert!(tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(recursive_sum(&board, 10_000).unwrap(), 50_005_000);
}

#[test]
fn removed_spool_claim_interrupts_a_running_recursive_read() {
    let mut board = board(true);
    let scratch = tp_core::test_fixtures::scratch("board-query-deadline-");
    let claim = scratch.path().join("request.claimed");
    std::fs::write(&claim, b"claimed").unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(30)).with_claim_path(claim.clone());
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        std::fs::remove_file(claim).unwrap();
    });
    let started = Instant::now();

    let error = with_query_deadline(&mut board, &deadline, |board| {
        recursive_sum(board, 50_000_000)
    })
    .unwrap_err();
    cancel.join().unwrap();

    assert!(tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn disconnected_peer_interrupts_a_running_recursive_read() {
    let mut board = board(false);
    let (peer, client) = UnixStream::pair().unwrap();
    let peer = Arc::new(peer);
    let deadline = QueryDeadline::after(Duration::from_secs(30)).with_peer(Arc::clone(&peer));
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        drop(client);
    });
    let started = Instant::now();

    let error = with_query_deadline(&mut board, &deadline, |board| {
        recursive_sum(board, 50_000_000)
    })
    .unwrap_err();
    cancel.join().unwrap();

    assert!(tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn progress_handler_is_cleared_during_unwind() {
    let mut board = board(true);
    let (peer, client) = UnixStream::pair().unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(30)).with_peer(Arc::new(peer));
    let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = with_query_deadline(
            &mut board,
            &deadline,
            |_: &mut LocalBoard| -> Result<(), BoardError> {
                panic!("injected read panic");
            },
        );
    }));
    assert!(panic.is_err());
    drop(client);

    assert_eq!(recursive_sum(&board, 10_000).unwrap(), 50_005_000);
}
