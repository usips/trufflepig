use super::*;

fn router_with_newer_storage(database: &Path) -> FakeGateway {
    let config = BoardConfig::for_database(database);
    drop(crate::board::local_board::LocalBoard::open(&config).unwrap());
    rusqlite::Connection::open(database)
        .unwrap()
        .pragma_update(None, "user_version", crate::board::SCHEMA_VERSION + 1)
        .unwrap();
    let error = crate::board::local_board::LocalBoard::open(&config)
        .err()
        .expect("newer storage is refused");
    let status = serde_json::json!({
        "status": "ok", "board_api": BOARD_API, "board_db": database
    })
    .to_string();
    FakeGateway {
        replies: VecDeque::from([Ok(Some(status)), Err(anyhow::anyhow!("daemon: {error}"))]),
        ..Default::default()
    }
}

#[test]
fn router_schema_refusal_gives_upgrade_advice() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = router_with_newer_storage(&database);
    let error = invoke(
        &["board", "show"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        None,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("upgrade trufflepig"),
        "{error:#}"
    );
    assert!(!error.to_string().contains("system ensure"), "{error:#}");
    assert_eq!(gateway.requests.len(), 2);
    assert_eq!(gateway.ensured, 0);
}

#[test]
fn router_schema_refusal_queues_feedback_for_an_upgrade() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = router_with_newer_storage(&database);
    invoke(
        &["feedback", "blocked", "newer storage"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        None,
    )
    .unwrap();
    let files: Vec<_> = std::fs::read_dir(directory.path().join("spool"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].extension().unwrap(), "feedback");
    assert_eq!(gateway.requests.len(), 2);
    assert_eq!(gateway.ensured, 0);
}
