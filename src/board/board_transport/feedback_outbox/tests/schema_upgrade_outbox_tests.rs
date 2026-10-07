use super::*;
use crate::board::{BoardConfig, SCHEMA_VERSION, local_board::LocalBoard};

#[test]
fn schema_upgrade_keeps_queued_feedback_pending() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let config = BoardConfig::for_database(&database);
    drop(LocalBoard::open(&config).unwrap());
    rusqlite::Connection::open(&database)
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    let error = LocalBoard::open(&config)
        .err()
        .expect("newer schema is refused");
    let spool = directory.path().join("spool");
    queue(&spool, &report()).unwrap();
    let summary = import_pending(&spool, &mut RejectedImport(error.code)).unwrap();
    assert_eq!(summary.pending, 1);
    assert_eq!(summary.quarantined, 0);
    let remaining = fs::read_dir(&spool)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(remaining.extension().unwrap(), "feedback");
}
