use super::*;

#[test]
fn configured_repository_identity_conflicts_block_reads_and_writes_before_mutation() {
    let fixture = crate::board::repo_identity::tests::GitFixture::new();
    fixture.commit("portable root");
    fixture.git(&["config", "remote.origin.url", "https://example.test/repo"]);
    let database = fixture.directory.path().join("board.sqlite3");
    let mut config = BoardConfig::for_database(&database);
    let host = BoardHost::with_config(config.clone());
    let run = |words: &[&str]| {
        let mut args = vec![
            "--root".to_owned(),
            fixture.root.to_string_lossy().into_owned(),
        ];
        args.extend(words.iter().map(|word| (*word).to_owned()));
        host.run(
            &crate::cli::parse(&args).unwrap(),
            &RequestContext::new(None, None),
            QueryDeadline::start(),
        )
    };
    run(&["board", "new", "First identity"]).unwrap();
    let external = rusqlite::Connection::open(&database).unwrap();
    let existing: String = external
        .query_row("SELECT repo_key FROM repo_paths LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let conflicting = crate::board::board_ids::RepoKey::parse(&"f".repeat(40)).unwrap();
    assert_ne!(existing, conflicting.as_str());
    config
        .repos
        .insert("https://example.test/repo".into(), conflicting.clone());
    *host.inner.config.lock().unwrap() = BoardConfigCache::with_config(config);
    for words in [
        vec!["board", "new", "Must not mutate"],
        vec!["board", "show"],
        vec!["board", "review", "P1@1"],
        vec!["board", "inbox", "0"],
    ] {
        let error = run(&words).unwrap_err();
        assert_eq!(
            error.downcast_ref::<BoardError>().map(|error| error.code),
            Some(crate::board::board_protocol::BoardErrorCode::InvalidOptions)
        );
        assert!(error.to_string().contains(&existing));
        assert!(error.to_string().contains(conflicting.as_str()));
    }
    assert_eq!(
        external
            .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        external
            .query_row("SELECT repo_key FROM repo_paths LIMIT 1", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        existing
    );
}

#[test]
fn repository_failures_warn_once_for_writes_and_remain_visible_on_reads() {
    let fixture = crate::board::repo_identity::tests::GitFixture::new();
    fixture.commit("valid repository root");
    std::fs::write(
        fixture.root.join(".git/refs/heads/main"),
        "not-an-object-id\n",
    )
    .unwrap();
    let host = BoardHost::with_config(BoardConfig::for_database(
        fixture.directory.path().join("board.sqlite3"),
    ));
    let run = |words: &[&str]| {
        let mut args = vec![
            "--root".to_owned(),
            fixture.root.to_string_lossy().into_owned(),
        ];
        args.extend(words.iter().map(|word| (*word).to_owned()));
        let options = crate::cli::parse(&args).unwrap();
        host.run(
            &options,
            &RequestContext::new(None, None),
            QueryDeadline::start(),
        )
        .unwrap()
    };
    assert!(run(&["board", "new", "First write"]).contains("repository registration"));
    assert!(!run(&["board", "new", "Second write"]).contains("repository registration"));
    assert!(run(&["board", "show"]).contains("repository registration"));
    assert!(run(&["board", "review", "P1@1"]).contains("repository registration"));
}
