use super::*;

#[test]
fn bootstrap_line_carries_the_token_only_to_a_terminal() {
    let directory = crate::board::board_test_support::scratch("web-bootstrap-");
    let token =
        web_guard::BoardWebToken::rotate_at(&directory.path().join("board-web.token")).unwrap();
    let guard = WebGuard::with_token("127.0.0.1:7341".parse().unwrap(), token).unwrap();
    let terminal = bootstrap_line(&guard, true);
    assert_eq!(terminal, format!("board web: {}", guard.bootstrap_url()));
    assert!(terminal.contains("#token="));
    let piped = bootstrap_line(&guard, false);
    assert!(piped.contains(guard.origin()), "{piped}");
    assert!(piped.contains("trufflepig board web"), "{piped}");
    assert!(!piped.contains("#token="), "{piped}");
    assert!(
        !piped.contains(guard.bootstrap_url().split_once("#token=").unwrap().1),
        "{piped}"
    );
}

#[test]
fn web_new_plan_links_a_chosen_repository_and_lists_repository_keys() {
    let directory = crate::board::board_test_support::scratch("web-repo-link-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config.clone()),
        directory.path().join("runtime"),
    )
    .unwrap();
    let repo = RepoKey::parse(&"e".repeat(40)).unwrap();
    let seeded = rusqlite::Connection::open(&config.db_path).unwrap();
    seeded
        .execute("INSERT INTO repos(repo_key) VALUES(?1)", [repo.as_str()])
        .unwrap();
    seeded
        .execute(
            "INSERT INTO repo_paths(repo_key,host,common_dir,root_commits_json) VALUES(?1,'laptop','/repo','[]')",
            [repo.as_str()],
        )
        .unwrap();
    drop(seeded);
    let created = read(
        &store,
        BoardOp::New {
            title: PlanTitle::new("Web plan").unwrap(),
            body: PlanText::new("").unwrap(),
            steward: None,
            repo_key: Some(repo.clone()),
        },
    );
    let BoardResult::Change(created) = created.result else {
        panic!("expected plan receipt")
    };
    let plan = created.plan.unwrap();
    let linked: String = rusqlite::Connection::open(&config.db_path)
        .unwrap()
        .query_row(
            "SELECT repo_key FROM plan_repos WHERE plan_id=?1",
            [plan.get() as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(linked, repo.as_str(), "the new plan links the chosen repo");
    let reply = read(&store, BoardOp::Repositories { plan: None });
    assert!(
        matches!(
            &reply.result,
            BoardResult::Repositories(targets)
                if targets.iter().any(|target| target.registration.repo_key == repo)
        ),
        "the web board exposes repository keys for the New-plan picker"
    );
}

#[test]
fn standalone_web_store_reads_and_writes_without_a_router() {
    let directory = crate::board::board_test_support::scratch("web-standalone-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config),
        directory.path().join("absent-runtime"),
    )
    .unwrap();
    read(
        &store,
        BoardOp::New {
            title: PlanTitle::new("Local board").unwrap(),
            body: PlanText::new("ready\n").unwrap(),
            steward: None,
            repo_key: None,
        },
    );
    let reply = read(&store, overview());
    assert!(matches!(reply.result, BoardResult::Overview(page) if page.plans.len() == 1));
}

#[test]
fn panicked_web_write_rolls_back_and_reopens_before_next_write() {
    let directory = crate::board::board_test_support::scratch("web-writer-panic-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config),
        directory.path().join("runtime"),
    )
    .unwrap();
    store
        .writer
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .inject_panic_after_write();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read(
            &store,
            BoardOp::New {
                title: PlanTitle::new("Uncommitted").unwrap(),
                body: PlanText::new("must disappear\n").unwrap(),
                steward: None,
                repo_key: None,
            },
        )
    }));
    assert!(panic.is_err());
    assert!(store.writer.is_poisoned());
    let empty = read(&store, overview());
    assert!(matches!(empty.result, BoardResult::Overview(page) if page.plans.is_empty()));
    read(
        &store,
        BoardOp::New {
            title: PlanTitle::new("Recovered").unwrap(),
            body: PlanText::new("durable\n").unwrap(),
            steward: None,
            repo_key: None,
        },
    );
    assert!(!store.writer.is_poisoned());
    let reply = read(&store, overview());
    assert!(matches!(reply.result, BoardResult::Overview(page)
        if page.plans.len() == 1 && page.plans[0].plan.title.as_str() == "Recovered"));
}

#[test]
fn changed_router_marker_refuses_web_writes_after_startup() {
    let directory = crate::board::board_test_support::scratch("web-marker-");
    let runtime = directory.path().join("runtime");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config.clone()),
        runtime.clone(),
    )
    .unwrap();
    store
        .config(Instant::now() + Duration::from_secs(1))
        .unwrap();
    crate::system::record_board_database(&runtime, &directory.path().join("router.sqlite3"))
        .unwrap();
    let error = web_ops::execute(
        &store,
        WebRequest {
            api: BOARD_API,
            op: BoardOp::New {
                title: PlanTitle::new("must not commit").unwrap(),
                body: PlanText::new("").unwrap(),
                steward: None,
                repo_key: None,
            },
        },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
    let request = BoardRequest::new(
        config.actor(Some("human"), Some("web")).unwrap(),
        overview(),
    );
    let reply = store
        .readers
        .with_reader(&config, Instant::now() + Duration::from_secs(1), |reader| {
            reader.handle(&request)
        })
        .unwrap();
    assert!(matches!(reply.result, BoardResult::Overview(page) if page.plans.is_empty()));
}
