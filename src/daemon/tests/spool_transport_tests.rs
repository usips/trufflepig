use super::*;

#[test]
fn spool_round_trip_answers_request_and_cleans_up() {
    let scratch = scratch();
    let dir = scratch.path().join("spool");
    let mut server = spool::SpoolServer::open(&dir).unwrap();
    assert!(server.claim().is_empty());
    let context = crate::diagnostics::RequestContext::new(None, None);
    let args = vec!["status".to_owned()];
    let client = {
        let (dir, context) = (dir.clone(), context.clone());
        std::thread::spawn(move || {
            spool::request(
                &dir,
                &args,
                &context,
                QueryDeadline::after(super::CLIENT_REPLY_WAIT),
            )
        })
    };
    let request = dir.join(format!("{}.request", context.request_id));
    while !request.exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
    for claimed in server.claim() {
        claimed.answer(|context, args| Ok(format!("{}:{}", context.request_id, args.join(" "))));
    }
    let reply = client.join().unwrap().unwrap();
    assert_eq!(reply, Some(format!("{}:status", context.request_id)));
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from("heartbeat")]);
}

#[test]
fn spool_claims_each_request_once_until_answered() {
    let scratch = scratch();
    let dir = scratch.path().join("spool");
    let mut server = spool::SpoolServer::open(&dir).unwrap();
    let context = crate::diagnostics::RequestContext::new(None, None);
    let client = {
        let (dir, context) = (dir.clone(), context.clone());
        std::thread::spawn(move || {
            spool::request(
                &dir,
                &["refs".to_owned()],
                &context,
                QueryDeadline::after(super::CLIENT_REPLY_WAIT),
            )
        })
    };
    let request = dir.join(format!("{}.request", context.request_id));
    while !request.exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut claimed = server.claim();
    assert_eq!(claimed.len(), 1);
    // A later maintenance tick sees the claim, not a second pending request.
    assert!(server.claim().is_empty());
    assert!(dir.join(format!("{}.claimed", context.request_id)).exists());
    claimed.pop().unwrap().refuse(DAEMON_BUSY);
    let error = client.join().unwrap().unwrap_err();
    assert!(error.to_string().contains(DAEMON_BUSY), "{error:#}");
}

#[test]
fn spool_without_heartbeat_reports_no_daemon() {
    let scratch = scratch();
    let dir = scratch.path().join("spool");
    let context = crate::diagnostics::RequestContext::new(None, None);
    assert_eq!(
        spool::request(
            &dir,
            &["status".to_owned()],
            &context,
            QueryDeadline::after(super::CLIENT_REPLY_WAIT),
        )
        .unwrap(),
        None
    );
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("heartbeat"), b"").unwrap();
    let stale = SystemTime::now() - Duration::from_secs(60);
    File::open(dir.join("heartbeat"))
        .unwrap()
        .set_modified(stale)
        .unwrap();
    assert_eq!(
        spool::request(
            &dir,
            &["status".to_owned()],
            &context,
            QueryDeadline::after(super::CLIENT_REPLY_WAIT),
        )
        .unwrap(),
        None
    );
    assert!(fs::read_dir(&dir).unwrap().count() == 1);
}

#[test]
fn spool_reports_handler_failure_and_ignores_foreign_files() {
    let scratch = scratch();
    let dir = scratch.path().join("spool");
    let mut server = spool::SpoolServer::open(&dir).unwrap();
    fs::write(dir.join("notes.txt"), b"keep").unwrap();
    let context = crate::diagnostics::RequestContext::new(None, None);
    let client = {
        let (dir, context) = (dir.clone(), context.clone());
        std::thread::spawn(move || {
            spool::request(
                &dir,
                &["search".to_owned()],
                &context,
                QueryDeadline::after(super::CLIENT_REPLY_WAIT),
            )
        })
    };
    let request = dir.join(format!("{}.request", context.request_id));
    while !request.exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
    for claimed in server.claim() {
        claimed.answer(|_, _| anyhow::bail!("boom"));
    }
    let error = client.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("boom"), "{error:#}");
    assert!(dir.join("notes.txt").exists());
}

#[test]
fn spool_client_gives_up_when_the_claiming_router_dies() {
    let scratch = scratch();
    let dir = scratch.path().join("spool");
    let mut server = spool::SpoolServer::open(&dir).unwrap();
    let context = crate::diagnostics::RequestContext::new(None, None);
    let client = {
        let (dir, context) = (dir.clone(), context.clone());
        std::thread::spawn(move || {
            spool::request(
                &dir,
                &["refs".to_owned()],
                &context,
                QueryDeadline::after(super::CLIENT_REPLY_WAIT),
            )
        })
    };
    let request = dir.join(format!("{}.request", context.request_id));
    while !request.exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server.claim().len(), 1);
    // The router dies with the claim in hand: its heartbeat goes stale.
    File::open(dir.join("heartbeat"))
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(60))
        .unwrap();
    let started = Instant::now();
    let error = client.join().unwrap().unwrap_err();
    assert!(
        error.to_string().contains("daemon_unavailable"),
        "{error:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!dir.join(format!("{}.claimed", context.request_id)).exists());
}

#[test]
fn spool_client_deadline_expires_while_fresh_heartbeat_is_not_drained() {
    let scratch = scratch();
    let dir = scratch.path().join("spool");
    let _server = spool::SpoolServer::open(&dir).unwrap();
    let context = crate::diagnostics::RequestContext::new(None, None);
    let request = dir.join(format!("{}.request", context.request_id));
    let deadline = QueryDeadline::after(Duration::from_millis(200));

    // The heartbeat is fresh, but no maintenance tick claims the request.
    let started = Instant::now();
    let error = spool::request(&dir, &["status".to_owned()], &context, deadline).unwrap_err();

    assert!(deadline::is_timed_out(&error), "{error:#}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "spool client exceeded its injected deadline: {:?}",
        started.elapsed()
    );
    assert!(!request.exists(), "timed out request was left pending");
}
