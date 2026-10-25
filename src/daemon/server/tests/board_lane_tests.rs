use super::*;
use tp_board::board_backend::BOARD_INBOX_WAITER_LIMIT;

/// Router lanes come from `lane_size`; the query pool is irrelevant here.
fn router(handler: TestDaemon) -> Arc<Served> {
    Arc::new(Served::start_tier(
        AdmissionTier::Router,
        false,
        WIDE,
        handler,
    ))
}

/// Serves board argv from a real host on a scratch database, as the router
/// does; waits are capped so parked waiters time out after about 4 s.
fn board_router(host: tp_board::BoardHost) -> Arc<Served> {
    router(TestDaemon {
        reply: Box::new(move |request| {
            let options = crate::cli::parse(&request.args)?;
            let session = if options.words.get(1).is_some_and(|verb| verb == "hello") {
                "writer"
            } else {
                "waiter"
            };
            let context = RequestContext::new(Some(session.into()), Some("codex".into()));
            host.run(
                &options.board_arguments(),
                &context,
                request.deadline.capped(Duration::from_secs(6)),
            )
        }),
        reconcile: quiet(),
    })
}

#[test]
fn parked_inbox_waiters_leave_board_workers_for_writes() {
    let scratch = scratch();
    // The board creates its missing directory privately.
    let config =
        tp_board::BoardConfig::for_database(scratch.path().join("board").join("board.sqlite3"));
    let host = tp_board::BoardHost::with_config(config);
    // Create storage first so concurrent waiters never race its migration.
    host.ensure_writer(QueryDeadline::after(Duration::from_secs(5)))
        .unwrap();
    let served = board_router(host.clone());
    let wait = ["--format", "lines", "board", "inbox", "--wait", "--all"];
    let waiters: Vec<_> = (0..BOARD_INBOX_WAITER_LIMIT)
        .map(|_| {
            let served = Arc::clone(&served);
            std::thread::spawn(move || served.ask(&wait))
        })
        .collect();
    let started = Instant::now();
    while host.parked_inbox_waiters() < BOARD_INBOX_WAITER_LIMIT {
        if let Some(index) = waiters.iter().position(|waiter| waiter.is_finished()) {
            let mut waiters = waiters;
            panic!(
                "waiter returned before parking: {:?}",
                waiters.remove(index).join()
            );
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waiters never parked"
        );
        std::thread::yield_now();
    }

    let extra = served.ask(&wait).unwrap().unwrap();
    assert!(extra.contains("wait=busy"), "{extra}");
    let root = scratch.path().display().to_string();
    let hello = served
        .ask(&[
            "--format", "lines", "--root", &root, "board", "hello", "opus",
        ])
        .unwrap()
        .unwrap();
    assert!(hello.starts_with("hello "), "{hello}");
    assert_eq!(
        host.parked_inbox_waiters(),
        BOARD_INBOX_WAITER_LIMIT,
        "the write waited for parked inbox waiters to leave their workers"
    );
    for waiter in waiters {
        waiter.join().unwrap().unwrap();
    }
    Arc::into_inner(served).unwrap().stop().unwrap();
}

#[test]
fn status_preflight_answers_while_every_board_worker_is_held() {
    let (entered, entries) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (entered, released) = (Mutex::new(entered), Mutex::new(released));
    let served = router(TestDaemon {
        reply: Box::new(move |request| {
            if request.args.first().is_some_and(|verb| verb == "board") {
                entered.lock().unwrap().send(()).unwrap();
                let _ = released.lock().unwrap().recv();
            }
            Ok(request.args.join("|"))
        }),
        reconcile: quiet(),
    });
    let held: Vec<_> = (0..8)
        .map(|index| {
            let served = Arc::clone(&served);
            std::thread::spawn(move || {
                served.ask(&["board", "post", "P1", "note", &index.to_string()])
            })
        })
        .collect();
    for _ in 0..8 {
        entries
            .recv_timeout(Duration::from_secs(5))
            .expect("board lane never filled its workers");
    }

    let (status_tx, status_rx) = mpsc::channel();
    let status = {
        let served = Arc::clone(&served);
        std::thread::spawn(move || {
            let _ = status_tx.send(served.ask(&["system", "status"]));
        })
    };
    let answered = status_rx.recv_timeout(Duration::from_secs(5));
    for _ in 0..8 {
        release.send(()).unwrap();
    }
    status.join().unwrap();
    assert_eq!(
        answered
            .expect("status preflight waited behind the board lane")
            .unwrap()
            .as_deref(),
        Some("system|status")
    );
    for thread in held {
        thread.join().unwrap().unwrap();
    }
    Arc::into_inner(served).unwrap().stop().unwrap();
}
