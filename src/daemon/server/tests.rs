use super::super::tests::scratch;
use super::super::{SOCKET_NAME, request, stop};
use super::*;
use crate::diagnostics::RequestContext;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Barrier;
use std::time::Instant;

type Reply = Box<dyn Fn(&AcceptedRequest) -> Result<String> + Send + Sync>;
type Reconcile = Box<dyn Fn() -> Result<()> + Send + Sync>;

/// A handler scripted by closures.
struct TestDaemon {
    reply: Reply,
    reconcile: Reconcile,
}

impl DaemonHandler for TestDaemon {
    fn request(&self, request: AcceptedRequest) -> Result<String> {
        (self.reply)(&request)
    }
    fn reconcile(&self) -> Result<()> {
        (self.reconcile)()
    }
}

fn echo() -> Reply {
    Box::new(|request| Ok(request.args.join("|")))
}

fn quiet() -> Reconcile {
    Box::new(|| Ok(()))
}

struct Served {
    _scratch: tempfile::TempDir,
    root: PathBuf,
    cache: PathBuf,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
}

impl Served {
    fn start(watching: bool, pool: PoolSize, handler: TestDaemon) -> Self {
        let scratch = scratch();
        let root = scratch.path().join("repo");
        let cache = scratch.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        let (serve_root, serve_cache) = (root.clone(), cache.clone());
        let thread = std::thread::spawn(move || {
            let profile = ServeProfile {
                name: "test",
                watching,
                pool,
                spool: None,
            };
            serve(&serve_root, &serve_cache, profile, handler)
        });
        let started = Instant::now();
        while !cache.join(SOCKET_NAME).exists() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "daemon never bound"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        Self {
            _scratch: scratch,
            root,
            cache,
            thread: Some(thread),
        }
    }

    fn ask(&self, args: &[&str]) -> Result<Option<String>> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        request(&self.cache, &args, &RequestContext::new(None, None))
    }

    fn stop(mut self) -> Result<()> {
        stop(&self.cache)?;
        self.thread.take().unwrap().join().unwrap()
    }
}

const WIDE: PoolSize = PoolSize {
    workers: 8,
    queue: 64,
};

#[test]
fn daemon_serves_recovers_protocol_errors_watches_and_stops() {
    let (sender, receiver) = mpsc::channel();
    let sender = Mutex::new(sender);
    let served = Served::start(
        true,
        WIDE,
        TestDaemon {
            reply: echo(),
            reconcile: Box::new(move || {
                let _ = sender.lock().unwrap().send(());
                Ok(())
            }),
        },
    );
    receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut bad_client = UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap();
    bad_client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    bad_client
        .write_all(&((protocol::REQUEST_LIMIT + 1) as u32).to_be_bytes())
        .unwrap();
    let bad_reply = protocol::read_reply(&mut bad_client);
    let reply = served.ask(&["search", "a\nb"]);
    std::fs::write(served.root.join("changed.rs"), "fn changed() {}\n").unwrap();
    let watched = receiver.recv_timeout(Duration::from_secs(5));
    let cache = served.cache.clone();
    served.stop().unwrap();
    assert!(matches!(bad_reply.unwrap(), DaemonReply::Failure { .. }));
    assert_eq!(reply.unwrap().as_deref(), Some("search|a\nb"));
    assert!(watched.is_ok(), "watcher did not trigger reconciliation");
    assert_eq!(
        request(&cache, &[], &RequestContext::new(None, None)).unwrap(),
        None
    );
}

#[test]
fn daemon_exits_when_root_is_removed() {
    let (reconciled, reconciles) = mpsc::channel();
    let reconciled = Mutex::new(reconciled);
    let mut served = Served::start(
        true,
        WIDE,
        TestDaemon {
            reply: echo(),
            reconcile: Box::new(move || {
                let _ = reconciled.lock().unwrap().send(());
                Ok(())
            }),
        },
    );
    reconciles.recv_timeout(Duration::from_secs(5)).unwrap();
    std::fs::remove_dir_all(&served.root).unwrap();
    let thread = served.thread.take().unwrap();
    let started = Instant::now();
    while !thread.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "daemon did not exit after its root was removed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    thread.join().unwrap().unwrap();
    assert!(!served.cache.join(SOCKET_NAME).exists());
    assert!(
        DaemonSocket::bind(&served.cache).is_ok(),
        "daemon.lock still held"
    );
}

#[test]
fn concurrent_clients_are_answered_in_parallel() {
    let served = Served::start(
        false,
        WIDE,
        TestDaemon {
            reply: Box::new(|request| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(request.args.join("|"))
            }),
            reconcile: quiet(),
        },
    );
    let served = Arc::new(served);
    let barrier = Arc::new(Barrier::new(8));
    let started = Instant::now();
    let clients: Vec<_> = (0..8)
        .map(|client| {
            let (served, barrier) = (Arc::clone(&served), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                served.ask(&["refs", &client.to_string()])
            })
        })
        .collect();
    for (client, thread) in clients.into_iter().enumerate() {
        let reply = thread.join().unwrap().unwrap();
        assert_eq!(reply, Some(format!("refs|{client}")));
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "8 clients took {elapsed:?}"
    );
    Arc::into_inner(served).unwrap().stop().unwrap();
}

#[test]
fn requests_are_answered_while_the_initial_reconcile_runs() {
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let served = Served::start(
        true,
        WIDE,
        TestDaemon {
            reply: echo(),
            reconcile: Box::new(move || {
                // Blocks the first (initial) reconcile until the test releases it.
                let _ = released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10));
                Ok(())
            }),
        },
    );
    let started = Instant::now();
    assert_eq!(served.ask(&["status"]).unwrap().as_deref(), Some("status"));
    assert!(started.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    served.stop().unwrap();
}

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
fn router_answers_spooled_requests_on_its_workers() {
    let scratch = scratch();
    let (dir, spool) = (scratch.path().join("router"), scratch.path().join("spool"));
    let (serve_dir, serve_spool) = (dir.clone(), spool.clone());
    let router = std::thread::spawn(move || {
        let profile = ServeProfile {
            name: "test-router",
            watching: false,
            pool: WIDE,
            spool: Some(&serve_spool),
        };
        let handler = TestDaemon {
            reply: echo(),
            reconcile: quiet(),
        };
        serve(Path::new("/"), &serve_dir, profile, handler)
    });
    let started = Instant::now();
    while !spool.join("heartbeat").exists() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "spool never opened"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let clients: Vec<_> = (0..4)
        .map(|client| {
            let spool = spool.clone();
            std::thread::spawn(move || {
                let args = vec!["search".to_owned(), client.to_string()];
                super::super::spool::request(&spool, &args, &RequestContext::new(None, None))
            })
        })
        .collect();
    for (client, thread) in clients.into_iter().enumerate() {
        assert_eq!(
            thread.join().unwrap().unwrap(),
            Some(format!("search|{client}"))
        );
    }
    stop(&dir).unwrap();
    router.join().unwrap().unwrap();
}
