use super::*;
use crate::board::{
    board_ids::{BoardRef, EventSeq, RepoKey},
    board_protocol::{BOARD_API, BoardOp, BoardReply, BoardRequest, BoardResult, ClaimResume},
    board_vocabulary::{EntryText, PlanText, PlanTitle},
};
use event_stream::{EventStreams, SequencePoller};
use std::{
    io::{self, Read},
    net::{Shutdown, TcpListener, TcpStream},
    sync::mpsc,
};
use web_guard::{BoardWebToken, WebGuard};
use web_ops::WebRequest;

fn accept_fixture() -> (tempfile::TempDir, Arc<WebState>) {
    let directory = crate::board::board_test_support::scratch("web-accept-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config),
        directory.path().join("runtime"),
    )
    .unwrap();
    let token = BoardWebToken::rotate_at(&directory.path().join("board-web.token")).unwrap();
    let guard = WebGuard::with_token("127.0.0.1:7341".parse().unwrap(), token).unwrap();
    let poller = SequencePoller::start(Arc::new(|| Ok(EventSeq::new(0)))).unwrap();
    let streams = EventStreams::new(
        Arc::new(|_, _, _| {
            Ok(event_stream::ReplayBatch {
                latest: EventSeq::new(0),
                events: vec![],
            })
        }),
        poller.handle(),
    );
    let state = Arc::new(WebState {
        store: Arc::new(store),
        guard,
        streams,
        ingest: Default::default(),
    });
    (directory, state)
}

#[test]
fn accept_error_classification_retries_only_transient_failures() {
    for raw in [
        libc::ECONNABORTED,
        libc::ENFILE,
        libc::EMFILE,
        libc::ENOBUFS,
        libc::ENOMEM,
    ] {
        assert!(transient_accept_error(&io::Error::from_raw_os_error(raw)), "{raw}");
    }
    for kind in [
        io::ErrorKind::Interrupted,
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::TimedOut,
    ] {
        assert!(transient_accept_error(&io::Error::new(kind, "transient")), "{kind:?}");
    }
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::AddrInUse,
    ] {
        assert!(!transient_accept_error(&io::Error::new(kind, "fatal")), "{kind:?}");
    }
}

#[test]
fn transient_accept_errors_do_not_stop_the_server() {
    let (_directory, state) = accept_fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    client.write_all(b"GARBAGE\r\n\r\n").unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let (tx, rx) = mpsc::channel::<io::Result<TcpStream>>();
    tx.send(Err(io::Error::from_raw_os_error(libc::ECONNABORTED)))
        .unwrap();
    tx.send(Err(io::Error::from_raw_os_error(libc::ENFILE))).unwrap();
    tx.send(Ok(server)).unwrap();
    let worker = {
        let state = Arc::clone(&state);
        std::thread::spawn(move || serve_connections(rx.into_iter(), &state))
    };
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 400 "), "{reply}");
    tx.send(Err(io::Error::new(io::ErrorKind::PermissionDenied, "fatal")))
        .unwrap();
    let error = worker.join().unwrap().unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().map(io::Error::kind),
        Some(io::ErrorKind::PermissionDenied)
    );
}

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
                    resume: ClaimResume::No,
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

fn stream_socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    (listener.accept().unwrap().0, client)
}

fn read_frame_until(client: &mut TcpStream, needle: &str) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    while !String::from_utf8_lossy(&bytes).contains(needle) {
        let count = client.read(&mut buffer).unwrap();
        assert!(count > 0, "stream closed before {needle}");
        bytes.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8(bytes).unwrap()
}

fn subscribe(streams: &EventStreams) -> TcpStream {
    let (server, client) = stream_socket_pair();
    streams
        .spawn(
            server,
            event_stream::StreamRequest::parse(None, None, None).unwrap(),
            streams.reserve().unwrap(),
        )
        .unwrap();
    client
}

#[test]
fn event_streams_share_a_dedicated_connection_outside_the_reader_pool() {
    let directory = crate::board::board_test_support::scratch("web-stream-feed-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = Arc::new(
        WebStore::open_at(
            BoardConfigCache::with_config(config.clone()),
            directory.path().join("runtime"),
        )
        .unwrap(),
    );
    for title in ["First plan", "Second plan"] {
        read(
            &store,
            BoardOp::New {
                title: PlanTitle::new(title).unwrap(),
                body: PlanText::new("seed\n").unwrap(),
                steward: None,
                repo_key: None,
            },
        );
    }
    let (poller, streams) = open_stream_feed(&store).unwrap();
    assert_eq!(store.readers.checkout_count(), 0, "writes use the writer");
    let mut first = subscribe(&streams);
    let mut second = subscribe(&streams);
    let replay_a = read_frame_until(&mut first, "id: 2\n");
    let replay_b = read_frame_until(&mut second, "id: 2\n");
    assert!(replay_a.contains("id: 1\n"), "{replay_a}");
    assert_eq!(
        replay_a.split("\r\n\r\n").nth(1),
        replay_b.split("\r\n\r\n").nth(1),
        "both subscribers replay the same ring history"
    );
    assert_eq!(
        store.readers.checkout_count(),
        0,
        "stream replay never checks out a pooled reader"
    );
    for _ in 0..3 {
        let reply = read(&store, overview());
        assert!(matches!(reply.result, BoardResult::Overview(_)));
    }
    assert_eq!(
        store.readers.checkout_count(),
        3,
        "only the GETs drew from the four-connection pool"
    );
    assert_eq!(
        store.readers.pool_size(),
        4,
        "the dedicated feed connection leaves the request pool whole"
    );
    read(
        &store,
        BoardOp::New {
            title: PlanTitle::new("Live plan").unwrap(),
            body: PlanText::new("live\n").unwrap(),
            steward: None,
            repo_key: None,
        },
    );
    // The real 250 ms poller drives the write into both shared streams.
    assert!(read_frame_until(&mut first, "id: 3\n").contains("id: 3\n"));
    assert!(read_frame_until(&mut second, "id: 3\n").contains("id: 3\n"));
    assert_eq!(
        store.readers.checkout_count(),
        3,
        "the live fill used the dedicated feed connection"
    );
    drop(first);
    drop(second);
    let started = Instant::now();
    while streams.active() != 0 {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stream permit retained"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(streams);
    drop(poller);
}
