use super::*;
use crate::board::{
    board_ids::{BoardRef, RepoKey},
    board_protocol::{BOARD_API, BoardOp, BoardReply, BoardRequest, BoardResult},
    board_vocabulary::{EntryText, PlanText, PlanTitle},
};
use web_ops::WebRequest;

#[test]
fn bootstrap_line_carries_the_token_only_to_a_terminal() {
    let directory = tempfile::tempdir().unwrap();
    let token = web_guard::BoardWebToken::rotate_at(&directory.path().join("board-web.token"))
        .unwrap();
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

fn overview() -> BoardOp {
    BoardOp::Overview {
        repo_key: None,
        after: None,
        through: None,
        limit: 200,
    }
}

fn read(store: &WebStore, op: BoardOp) -> BoardReply {
    web_ops::execute(
        store,
        WebRequest { api: BOARD_API, op },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap()
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
        matches!(&reply.result, BoardResult::Repositories(targets) if targets.iter().any(|target| target.registration.repo_key == repo)),
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

fn assert_stale_readers(
    pool: &ReaderPool,
    config: &BoardConfig,
    request: &BoardRequest,
    remaining: usize,
) {
    if remaining == 0 {
        return;
    }
    pool.with_reader(config, Instant::now() + Duration::from_secs(1), |reader| {
        let reply = reader.handle(request)?;
        let BoardResult::Plan(view) = reply.result else {
            panic!("expected plan");
        };
        assert_eq!(view.claims.len(), 1);
        assert!(view.claims.iter().all(|claim| claim.stale));
        assert_stale_readers(pool, config, request, remaining - 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn config_reload_reaches_reader_ttl_and_attention_without_events() {
    let directory = crate::board::board_test_support::scratch("web-config-");
    let config_path = directory.path().join("board.toml");
    std::fs::write(&config_path, "user = 'owner'\nclaim_ttl_minutes = 120\n").unwrap();
    let defaults = BoardConfig::for_database(directory.path().join("board.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_source(config_path.clone(), defaults),
        directory.path().join("runtime"),
    )
    .unwrap();
    let config = store
        .config(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let created = read(
        &store,
        BoardOp::New {
            title: PlanTitle::new("Trial").unwrap(),
            body: PlanText::new("old\n").unwrap(),
            steward: None,
            repo_key: None,
        },
    );
    let BoardResult::Change(created) = created.result else {
        panic!("expected plan receipt");
    };
    let plan = created.plan.unwrap();
    let created = read(
        &store,
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Task").unwrap(),
            to: None,
            section: None,
        },
    );
    let BoardResult::Change(created) = created.result else {
        panic!("expected task receipt");
    };
    let task = created.task.unwrap();
    let actor = config.actor(Some("codex"), Some("worker")).unwrap();
    let proposal = {
        let mut slot = store.writer.lock().unwrap();
        let writer = slot.as_mut().unwrap();
        writer
            .handle(&BoardRequest::new(
                actor.clone(),
                BoardOp::ClaimTask {
                    task,
                    scope: Some(EntryText::new("scope").unwrap()),
                    resume: false,
                },
            ))
            .unwrap();
        writer
            .handle(&BoardRequest::new(
                actor,
                BoardOp::Propose {
                    base: crate::board::board_ids::PlanRevision::new(plan, 1).unwrap(),
                    body: PlanText::new("new\n").unwrap(),
                    summary: EntryText::new("proposal").unwrap(),
                    supersedes: None,
                },
            ))
            .unwrap()
    };
    let BoardResult::Change(proposal) = proposal.result else {
        panic!("expected proposal receipt");
    };
    rusqlite::Connection::open(&config.db_path)
        .unwrap()
        .execute("UPDATE claims SET last_active=last_active-120", [])
        .unwrap();
    let before = read(
        &store,
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
    );
    let BoardResult::Plan(view) = &before.result else {
        panic!("expected plan");
    };
    assert_eq!(view.claims.len(), 1);
    assert!(view.claims.iter().all(|claim| !claim.stale));
    let attention = || {
        serde_json::from_value(
            serde_json::json!({"op":"attention","all":true,"repo_key":null,"limit":200}),
        )
        .unwrap()
    };
    let original = serde_json::to_value(read(&store, attention())).unwrap();
    assert!(
        original["result"]["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == proposal.entry.to_string())
    );
    std::fs::write(&config_path, "user = 'reviewer'\nclaim_ttl_minutes = 1\n").unwrap();
    let refreshed = store
        .config
        .lock()
        .unwrap()
        .get(Instant::now() + Duration::from_secs(3))
        .unwrap();
    let changed = read(&store, attention());
    assert_eq!(changed.snapshot_seq, before.snapshot_seq);
    let changed = serde_json::to_value(changed).unwrap();
    assert_eq!(changed["result"]["data"]["actor"]["user"], "reviewer");
    assert!(
        !changed["result"]["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == proposal.entry.to_string())
    );
    let request = BoardRequest::new(
        refreshed.actor(Some("human"), Some("web")).unwrap(),
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
    );
    assert_stale_readers(&store.readers, &refreshed, &request, 4);
}
