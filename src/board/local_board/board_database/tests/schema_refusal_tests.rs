use super::*;
use crate::board::{board_grammar, unavailable_or_queue};
use std::cell::Cell;

thread_local! {
    static SCHEMA_UPGRADE_BEFORE_LOCK: Cell<Option<i64>> = const { Cell::new(None) };
}

pub(in crate::board::local_board::board_database) fn upgrade_schema_before_lock(path: &Path) {
    if let Some(version) = SCHEMA_UPGRADE_BEFORE_LOCK.with(|pending| pending.take()) {
        Connection::open(path)
            .unwrap()
            .pragma_update(None, "user_version", version)
            .unwrap();
    }
}

#[test]
fn schema_became_newer_gives_upgrade_advice() {
    let directory = directory();
    let database = directory.path().join("board.sqlite3");
    drop(open(&database).unwrap());
    SCHEMA_UPGRADE_BEFORE_LOCK.with(|pending| pending.set(Some(SCHEMA_VERSION + 1)));
    let error = open(&database).unwrap_err();
    assert!(error.message.contains("schema became newer"), "{error}");
    let config = crate::board::BoardConfig::for_database(&database);
    let args: Vec<String> = ["board", "inbox"].map(str::to_owned).into();
    let options = crate::cli::parse(&args).unwrap();
    let command = board_grammar::parse(&options, None).unwrap();
    let refusal = unavailable_or_queue(
        &command,
        &options,
        &crate::diagnostics::RequestContext::new(None, None),
        &config,
        directory.path(),
        anyhow::Error::new(error),
    )
    .unwrap_err();
    assert!(
        refusal.to_string().contains("upgrade trufflepig"),
        "{refusal:#}"
    );
    assert!(
        !refusal.to_string().contains("system ensure"),
        "{refusal:#}"
    );
}

#[test]
fn newer_storage_has_the_same_code_for_reads_and_writes() {
    let directory = directory();
    let database = directory.path().join("board.sqlite3");
    drop(open(&database).unwrap());
    Connection::open(&database)
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    for error in [
        open(&database).unwrap_err(),
        open_read_with_timeout(&database, Duration::from_secs(1)).unwrap_err(),
    ] {
        assert_eq!(error.code.as_str(), "schema_newer", "{error}");
    }
}
