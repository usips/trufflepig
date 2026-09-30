//! Refusal, panic, and shutdown behavior of a serving daemon.
use super::*;

#[test]
fn full_pool_answers_daemon_busy() {
    let (entered, entries) = mpsc::channel::<()>();
    let (release, released) = mpsc::channel::<()>();
    let (entered, released) = (Mutex::new(entered), Mutex::new(released));
    let served = Served::start(
        false,
        PoolSize {
            workers: 1,
            queue: 1,
        },
        TestDaemon {
            reply: Box::new(move |request| {
                if request.args[0] == "hold" {
                    entered.lock().unwrap().send(()).unwrap();
                    released
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10))?;
                }
                Ok(request.args.join("|"))
            }),
            reconcile: quiet(),
        },
    );
    let served = Arc::new(served);
    let holder = {
        let served = Arc::clone(&served);
        std::thread::spawn(move || served.ask(&["hold"]))
    };
    entries.recv_timeout(Duration::from_secs(5)).unwrap();
    // The worker is held; connections are accepted in order: one queues, the next is refused.
    let raw = |word: &str| {
        let mut stream = UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let request = DaemonRequest::Arguments {
            context: RequestContext::new(None, None),
            args: vec![word.to_owned()],
        };
        protocol::write_request(&mut stream, &request).unwrap();
        stream
    };
    let mut queued = raw("queued");
    let mut refused = raw("refused");
    match protocol::read_reply(&mut refused).unwrap() {
        DaemonReply::Failure { message } => assert_eq!(message, DAEMON_BUSY),
        DaemonReply::Success { output } => panic!("full pool answered {output}"),
    }
    release.send(()).unwrap();
    assert!(matches!(
        protocol::read_reply(&mut queued).unwrap(),
        DaemonReply::Success { output } if output == "queued"
    ));
    assert_eq!(holder.join().unwrap().unwrap().as_deref(), Some("hold"));
    Arc::into_inner(served).unwrap().stop().unwrap();
}

#[test]
fn failed_initial_reconcile_stops_the_daemon_with_its_error() {
    let scratch = scratch();
    let root = scratch.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let profile = ServeProfile {
        name: "test",
        watching: false,
        pool: WIDE,
        spool: None,
    };
    let handler = TestDaemon {
        reply: echo(),
        reconcile: Box::new(|| anyhow::bail!("cache belongs to another repository root")),
    };
    let error = serve(&root, &scratch.path().join("cache"), profile, handler).unwrap_err();
    assert!(
        format!("{error:#}").contains("initial repository reconciliation"),
        "{error:#}"
    );
    assert!(!scratch.path().join("cache").join(SOCKET_NAME).exists());
}

#[test]
fn panicking_request_answers_internal_error_and_keeps_the_worker() {
    let served = Served::start(
        false,
        PoolSize {
            workers: 1,
            queue: 4,
        },
        TestDaemon {
            reply: Box::new(|request| {
                assert!(request.args[0] != "boom", "handler bug");
                Ok(request.args.join("|"))
            }),
            reconcile: quiet(),
        },
    );
    let error = served.ask(&["boom"]).unwrap_err();
    assert!(
        format!("{error:#}").contains("internal_error: handler bug"),
        "{error:#}"
    );
    assert_eq!(served.ask(&["after"]).unwrap().as_deref(), Some("after"));
    served.stop().unwrap();
}

#[test]
fn panicking_reconcile_stops_the_daemon() {
    let scratch = scratch();
    let root = scratch.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let profile = ServeProfile {
        name: "test",
        watching: false,
        pool: WIDE,
        spool: None,
    };
    let handler = TestDaemon {
        reply: echo(),
        reconcile: Box::new(|| panic!("reconcile bug")),
    };
    let cache = scratch.path().join("cache");
    let error = serve(&root, &cache, profile, handler).unwrap_err();
    assert!(
        format!("{error:#}").contains("internal_error: daemon maintenance panicked: reconcile bug"),
        "{error:#}"
    );
    assert!(!cache.join(SOCKET_NAME).exists(), "socket left for nobody");
}

#[test]
fn stop_lets_accepted_requests_finish() {
    let (entered, entries) = mpsc::channel::<()>();
    let entered = Mutex::new(entered);
    let finished = Arc::new(AtomicBool::new(false));
    let handler_finished = Arc::clone(&finished);
    let mut served = Served::start(
        false,
        WIDE,
        TestDaemon {
            reply: Box::new(move |request| {
                if request.args[0] == "slow" {
                    entered.lock().unwrap().send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(300));
                    handler_finished.store(true, Ordering::Release);
                }
                Ok(request.args.join("|"))
            }),
            reconcile: quiet(),
        },
    );
    let cache = served.cache.clone();
    let slow = std::thread::spawn(move || {
        request(
            &cache,
            &["slow".to_owned()],
            &RequestContext::new(None, None),
        )
    });
    entries.recv_timeout(Duration::from_secs(5)).unwrap();
    stop(&served.cache).unwrap();
    served.thread.take().unwrap().join().unwrap().unwrap();
    assert!(
        finished.load(Ordering::Acquire),
        "serve returned before an accepted request finished"
    );
    assert_eq!(slow.join().unwrap().unwrap().as_deref(), Some("slow"));
}
