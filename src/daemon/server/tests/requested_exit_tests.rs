use super::*;
use std::sync::atomic::AtomicBool;

/// Blocks each request until released; asks to exit once `exit` is set.
struct ExitingDaemon {
    entered: Mutex<mpsc::Sender<()>>,
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    exit: Arc<AtomicBool>,
}

impl DaemonHandler for ExitingDaemon {
    fn request(&self, _request: AcceptedRequest) -> Result<String> {
        let _ = self.entered.lock().unwrap().send(());
        let (released, changed) = &*self.release;
        let mut released = released.lock().unwrap();
        while !*released {
            released = changed.wait(released).unwrap();
        }
        Ok("finished".into())
    }

    fn requested_exit(&self) -> Option<anyhow::Error> {
        self.exit
            .load(Ordering::Acquire)
            .then(|| anyhow::anyhow!("restart requested"))
    }
}

#[test]
fn requested_exit_stops_admission_then_drains_in_flight_work_and_returns_the_error() {
    let scratch = scratch();
    let root = scratch.path().join("repo");
    let cache = scratch.path().join("cache");
    std::fs::create_dir(&root).unwrap();
    let (entered, in_flight) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let exit = Arc::new(AtomicBool::new(false));
    let handler = ExitingDaemon {
        entered: Mutex::new(entered),
        release: Arc::clone(&release),
        exit: Arc::clone(&exit),
    };
    let (serve_root, serve_cache) = (root.clone(), cache.clone());
    let server = std::thread::spawn(move || {
        let profile = ServeProfile {
            name: "test",
            watching: false,
            pool: WIDE,
            tier: AdmissionTier::Router,
            spool: None,
        };
        serve(&serve_root, &serve_cache, profile, handler)
    });
    let started = Instant::now();
    while !cache.join(SOCKET_NAME).exists() {
        assert!(started.elapsed() < Duration::from_secs(5), "never bound");
        std::thread::sleep(Duration::from_millis(2));
    }
    let client_cache = cache.clone();
    let client = std::thread::spawn(move || {
        request(
            &client_cache,
            &["status".to_owned()],
            &RequestContext::new(None, None),
        )
    });
    in_flight.recv_timeout(Duration::from_secs(5)).unwrap();

    exit.store(true, Ordering::Release);
    let started = Instant::now();
    while cache.join(SOCKET_NAME).exists() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "an exiting daemon releases its socket before draining"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!server.is_finished(), "in-flight work is still draining");
    let refused = request(
        &cache,
        &["status".to_owned()],
        &RequestContext::new(None, None),
    );
    assert!(
        matches!(refused, Ok(None) | Err(_)),
        "no new work is admitted: {refused:?}"
    );

    let (released, changed) = &*release;
    *released.lock().unwrap() = true;
    changed.notify_all();
    assert_eq!(client.join().unwrap().unwrap().as_deref(), Some("finished"));
    let error = server.join().unwrap().unwrap_err();
    assert_eq!(error.to_string(), "restart requested");
}
