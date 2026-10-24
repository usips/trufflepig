use super::super::request_admission::RequestAdmission;
use super::*;
use crate::request_context::RequestContext;
use std::fs::File;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

fn one_worker() -> PoolSize {
    PoolSize {
        workers: 1,
        queue: 1,
    }
}

#[test]
fn active_maintenance_does_not_hold_the_query_lane() {
    let (entered, entries) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (entered, released) = (Mutex::new(entered), Mutex::new(released));
    let served = Served::start(
        false,
        one_worker(),
        TestDaemon {
            reply: Box::new(move |request| {
                if request
                    .args
                    .first()
                    .is_some_and(|verb| verb == "hist-index")
                {
                    entered.lock().unwrap().send(()).unwrap();
                    released.lock().unwrap().recv().unwrap();
                }
                Ok(request.args.join("|"))
            }),
            reconcile: quiet(),
        },
    );
    let served = Arc::new(served);
    let maintenance = {
        let served = Arc::clone(&served);
        std::thread::spawn(move || served.ask(&["hist-index", "--wait"]))
    };
    entries.recv_timeout(Duration::from_secs(5)).unwrap();

    let (query_tx, query_rx) = mpsc::channel();
    let query = {
        let (served, query_tx) = (Arc::clone(&served), query_tx.clone());
        std::thread::spawn(move || {
            let _ = query_tx.send(served.ask(&["status"]));
        })
    };
    let query_result = query_rx.recv_timeout(Duration::from_secs(5));
    release.send(()).unwrap();
    query.join().unwrap();
    assert_eq!(
        query_result
            .expect("query lane waited behind maintenance")
            .unwrap()
            .as_deref(),
        Some("status")
    );
    assert_eq!(
        maintenance.join().unwrap().unwrap().as_deref(),
        Some("hist-index|--wait")
    );
    Arc::into_inner(served).unwrap().stop().unwrap();
}

#[test]
fn full_query_lane_does_not_refuse_board_requests() {
    let (entered, entries) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (entered, released) = (Mutex::new(entered), Mutex::new(released));
    let held = AtomicUsize::new(0);
    let served = Arc::new(Served::start(
        false,
        one_worker(),
        TestDaemon {
            reply: Box::new(move |request| {
                if request.args.first().is_some_and(|verb| verb == "status")
                    && held.fetch_add(1, Ordering::AcqRel) == 0
                {
                    entered.lock().unwrap().send(()).unwrap();
                    released.lock().unwrap().recv().unwrap();
                }
                Ok(request.args.join("|"))
            }),
            reconcile: quiet(),
        },
    ));
    let active = {
        let served = Arc::clone(&served);
        std::thread::spawn(move || served.ask(&["status"]))
    };
    entries.recv_timeout(Duration::from_secs(5)).unwrap();
    let (results, replies) = mpsc::channel();
    let candidates: Vec<_> = (0..2)
        .map(|_| {
            let (served, results) = (Arc::clone(&served), results.clone());
            std::thread::spawn(move || {
                let _ = results.send(served.ask(&["status"]));
            })
        })
        .collect();
    drop(results);
    let refused = replies
        .recv_timeout(Duration::from_secs(5))
        .expect("query lane did not reach its bounded queue capacity");
    assert!(
        format!(
            "{:#}",
            refused.expect_err("a full query queue must refuse one")
        )
        .contains("daemon_busy"),
    );

    for args in [
        &["board", "show", "P7"][..],
        &["feedback", "ls"],
        &["system", "status"],
    ] {
        assert_eq!(
            served.ask(args).unwrap().as_deref(),
            Some(args.join("|").as_str()),
            "{args:?} waited behind the full query lane"
        );
    }
    release.send(()).unwrap();
    active.join().unwrap().unwrap();
    for candidate in candidates {
        candidate.join().unwrap();
    }
    Arc::into_inner(served).unwrap().stop().unwrap();
}

#[test]
fn full_regex_lane_does_not_hold_the_ordinary_query_lane() {
    let (entered, entries) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let entered_regex = AtomicUsize::new(0);
    let served = Served::start(
        false,
        one_worker(),
        TestDaemon {
            reply: Box::new(move |request| {
                if request
                    .args
                    .get(1)
                    .is_some_and(|query| query.starts_with("re:"))
                    && entered_regex.fetch_add(1, Ordering::AcqRel) < 2
                {
                    entered.send(()).unwrap();
                    released.lock().unwrap().recv().unwrap();
                }
                Ok(request.args.join("|"))
            }),
            reconcile: quiet(),
        },
    );
    let served = Arc::new(served);
    let active: Vec<_> = (0..2)
        .map(|_| {
            let served = Arc::clone(&served);
            std::thread::spawn(move || served.ask(&["search", "re:hold"]))
        })
        .collect();
    for _ in 0..2 {
        entries.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    let (results, replies) = mpsc::channel();
    let candidates: Vec<_> = (0..9)
        .map(|index| {
            let (served, results) = (Arc::clone(&served), results.clone());
            std::thread::spawn(move || {
                let args = vec![
                    "search".to_owned(),
                    "re:queued".to_owned(),
                    index.to_string(),
                ];
                let result = request(&served.cache, &args, &RequestContext::new(None, None));
                let _ = results.send((index, result));
            })
        })
        .collect();
    drop(results);

    let (_, refused) = replies
        .recv_timeout(Duration::from_secs(5))
        .expect("regex lane did not reach its bounded queue capacity");
    let error = refused.expect_err("a full regex queue must refuse one candidate");
    assert!(
        format!("{error:#}").contains("pool=test-regex queue_depth=8 queue_capacity=8"),
        "{error:#}"
    );

    let (query_tx, query_rx) = mpsc::channel();
    let query = {
        let (served, query_tx) = (Arc::clone(&served), query_tx.clone());
        std::thread::spawn(move || {
            let _ = query_tx.send(served.ask(&["search", "sym:ordinary"]));
        })
    };
    let query_result = query_rx.recv_timeout(Duration::from_secs(5));
    release.send(()).unwrap();
    release.send(()).unwrap();
    query.join().unwrap();
    assert_eq!(
        query_result
            .expect("ordinary query waited behind the regex lane")
            .unwrap()
            .as_deref(),
        Some("search|sym:ordinary")
    );
    for active in active {
        assert_eq!(
            active.join().unwrap().unwrap().as_deref(),
            Some("search|re:hold")
        );
    }

    let mut refused_count = 1;
    for thread in candidates {
        thread.join().unwrap();
    }
    while let Ok((_, result)) = replies.try_recv() {
        if result.is_err() {
            refused_count += 1;
        }
    }
    assert_eq!(refused_count, 1);
    Arc::into_inner(served).unwrap().stop().unwrap();
}

#[test]
fn expired_or_disconnected_queued_classes_skip_the_handler() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(TestDaemon {
        reply: {
            let calls = Arc::clone(&calls);
            Box::new(move |_| {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok("unexpected".to_owned())
            })
        },
        reconcile: quiet(),
    });
    let classes = [
        RequestClass {
            lane: RequestLane::Query,
            verb: "search",
        },
        RequestClass {
            lane: RequestLane::Maintenance,
            verb: "index",
        },
        RequestClass {
            lane: RequestLane::Regex,
            verb: "search",
        },
    ];

    for class in classes {
        let (daemon, mut client) = UnixStream::pair().unwrap();
        super::super::run_connection_request(
            Arc::new(daemon),
            QueryDeadline::after(Duration::ZERO),
            class,
            RequestContext::new(None, None),
            vec![class.verb.to_owned()],
            Arc::clone(&handler),
            None,
        );
        assert!(matches!(
            protocol::read_reply(&mut client).unwrap(),
            DaemonReply::Failure { message } if message.starts_with("timed_out:")
        ));

        let (daemon, client) = UnixStream::pair().unwrap();
        let peer = Arc::new(daemon);
        let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::clone(&peer));
        drop(client);
        super::super::run_connection_request(
            peer,
            deadline,
            class,
            RequestContext::new(None, None),
            vec![class.verb.to_owned()],
            Arc::clone(&handler),
            None,
        );
    }
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[test]
fn an_existing_typed_reply_timeout_survives_request_expiry() {
    let (daemon, mut client) = UnixStream::pair().unwrap();
    let scratch = scratch();
    let claim = scratch.path().join("typed-timeout.claimed");
    std::fs::write(&claim, b"claimed").unwrap();
    let (entered, entries) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let handler = Arc::new(TestDaemon {
        reply: {
            let (entered, released) = (Mutex::new(entered), Mutex::new(released));
            Box::new(move |request| {
                entered.lock().unwrap().send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
                assert!(request.deadline.expired());
                Err(anyhow::Error::new(crate::daemon::DaemonReplyTimeout::new(
                    Duration::from_millis(40),
                    Duration::from_millis(25),
                )))
            })
        },
        reconcile: quiet(),
    });
    let request_claim = claim.clone();
    let request = std::thread::spawn(move || {
        super::super::run_connection_request(
            Arc::new(daemon),
            QueryDeadline::after(Duration::from_secs(5)).with_claim_path(request_claim),
            RequestClass {
                lane: RequestLane::Query,
                verb: "search",
            },
            RequestContext::new(None, None),
            vec!["search".to_owned()],
            handler,
            None,
        );
    });
    entries.recv_timeout(Duration::from_secs(2)).unwrap();
    std::fs::remove_file(&claim).unwrap();
    release.send(()).unwrap();
    request.join().unwrap();
    assert!(matches!(
        protocol::read_reply(&mut client).unwrap(),
        DaemonReply::Failure { message } if message.starts_with("daemon_reply_timeout:")
    ));
}

#[test]
fn active_maintenance_keeps_its_result_after_deadline_cancellation() {
    let scratch = scratch();
    let claim = scratch.path().join("maintenance.claimed");
    std::fs::write(&claim, b"claimed").unwrap();
    let (daemon, mut client) = UnixStream::pair().unwrap();
    let (entered, entries) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let handler = Arc::new(TestDaemon {
        reply: {
            let (entered, released) = (Mutex::new(entered), Mutex::new(released));
            Box::new(move |_| {
                entered.lock().unwrap().send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
                Ok("maintenance completed".to_owned())
            })
        },
        reconcile: quiet(),
    });
    let request_claim = claim.clone();
    let request = std::thread::spawn(move || {
        super::super::run_connection_request(
            Arc::new(daemon),
            QueryDeadline::after(Duration::from_secs(5)).with_claim_path(request_claim),
            RequestClass {
                lane: RequestLane::Maintenance,
                verb: "index",
            },
            RequestContext::new(None, None),
            vec!["index".to_owned()],
            handler,
            None,
        );
    });
    entries.recv_timeout(Duration::from_secs(2)).unwrap();
    std::fs::remove_file(&claim).unwrap();
    release.send(()).unwrap();
    request.join().unwrap();
    assert!(matches!(
        protocol::read_reply(&mut client).unwrap(),
        DaemonReply::Success { output } if output == "maintenance completed"
    ));
}

#[test]
fn disconnecting_an_active_query_expires_its_deadline_and_releases_the_worker() {
    let (entered, entries) = mpsc::channel();
    let observed_cancel = Arc::new(AtomicBool::new(false));
    let served = Served::start(
        false,
        one_worker(),
        TestDaemon {
            reply: {
                let (entered, observed_cancel) =
                    (Mutex::new(entered), Arc::clone(&observed_cancel));
                Box::new(move |request| {
                    if request.args.first().is_some_and(|verb| verb == "hold") {
                        entered.lock().unwrap().send(()).unwrap();
                        let started = Instant::now();
                        while !request.deadline.expired()
                            && started.elapsed() < Duration::from_secs(2)
                        {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        observed_cancel.store(request.deadline.expired(), Ordering::Release);
                    }
                    Ok(request.args.join("|"))
                })
            },
            reconcile: quiet(),
        },
    );
    let served = Arc::new(served);
    let mut client = UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    protocol::write_request(
        &mut client,
        &DaemonRequest::Arguments {
            context: RequestContext::new(None, None),
            args: vec!["hold".to_owned()],
        },
    )
    .unwrap();
    entries.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(client);

    let (query_tx, query_rx) = mpsc::channel();
    let query = {
        let (served, query_tx) = (Arc::clone(&served), query_tx.clone());
        std::thread::spawn(move || {
            let _ = query_tx.send(served.ask(&["status"]));
        })
    };
    let result = query_rx.recv_timeout(Duration::from_secs(5));
    query.join().unwrap();
    assert!(observed_cancel.load(Ordering::Acquire));
    assert_eq!(
        result
            .expect("disconnected query did not release its worker")
            .unwrap()
            .as_deref(),
        Some("status")
    );
    Arc::into_inner(served).unwrap().stop().unwrap();
}

#[test]
fn old_spooled_request_waiting_in_a_lane_never_reaches_the_handler() {
    let scratch = scratch();
    let spool_dir = scratch.path().join("spool");
    let mut spool = super::super::super::spool::SpoolServer::open(&spool_dir).unwrap();
    let id = uuid::Uuid::new_v4();
    let request_path = spool_dir.join(format!("{id}.request"));
    let reply_path = spool_dir.join(format!("{id}.reply"));
    let request = DaemonRequest::Arguments {
        context: RequestContext::new(None, None),
        args: vec!["search".to_owned(), "sym:expired".to_owned()],
    };
    std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    // About 2 s of the 20 s root budget remain: classification passes, then the
    // request expires while it waits behind the blocked worker.
    File::open(&request_path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(18))
        .unwrap();

    let admission = Arc::new(
        RequestAdmission::new("spool-age-test", AdmissionTier::Root, one_worker()).unwrap(),
    );
    let query_pool = admission
        .execution(RequestClass {
            lane: RequestLane::Query,
            verb: "search",
        })
        .1;
    let (entered, entries) = mpsc::channel();
    let (release, released) = mpsc::channel();
    assert!(
        query_pool
            .submit(Box::new(move || {
                entered.send(()).unwrap();
                released.recv().unwrap();
            }))
            .is_ok()
    );
    entries.recv_timeout(Duration::from_secs(1)).unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(TestDaemon {
        reply: {
            let calls = Arc::clone(&calls);
            Box::new(move |_| {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok("unexpected".to_owned())
            })
        },
        reconcile: quiet(),
    });
    super::super::dispatch_spooled(&mut spool, &handler, &admission);
    // Keep the only worker blocked until the request's own budget has passed.
    std::thread::sleep(Duration::from_millis(2500));
    release.send(()).unwrap();
    assert!(admission.drain(Duration::from_secs(1)));
    assert_eq!(calls.load(Ordering::Acquire), 0);
    let reply: DaemonReply = serde_json::from_slice(&std::fs::read(reply_path).unwrap()).unwrap();
    assert!(matches!(
        reply,
        DaemonReply::Failure { message }
            if message.starts_with("timed_out: spooled request expired before dispatch")
    ));
    admission.shutdown();
}

#[test]
fn removing_an_active_spool_claim_cancels_its_query_deadline() {
    let scratch = scratch();
    let spool_dir = scratch.path().join("active-spool");
    let mut spool = super::super::super::spool::SpoolServer::open(&spool_dir).unwrap();
    let id = uuid::Uuid::new_v4();
    let request_path = spool_dir.join(format!("{id}.request"));
    let claim_path = spool_dir.join(format!("{id}.claimed"));
    let reply_path = spool_dir.join(format!("{id}.reply"));
    let request = DaemonRequest::Arguments {
        context: RequestContext::new(None, None),
        args: vec!["search".to_owned(), "sym:withdrawn".to_owned()],
    };
    std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();

    let admission = Arc::new(
        RequestAdmission::new("spool-withdraw-test", AdmissionTier::Root, one_worker()).unwrap(),
    );
    let (entered, entries) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let observed_cancel = Arc::new(AtomicBool::new(false));
    let handler = Arc::new(TestDaemon {
        reply: {
            let (entered, released, observed_cancel) = (
                Mutex::new(entered),
                Mutex::new(released),
                Arc::clone(&observed_cancel),
            );
            Box::new(move |request| {
                entered.lock().unwrap().send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
                observed_cancel.store(request.deadline.expired(), Ordering::Release);
                Ok("completed after withdrawal".to_owned())
            })
        },
        reconcile: quiet(),
    });
    super::super::dispatch_spooled(&mut spool, &handler, &admission);
    entries.recv_timeout(Duration::from_secs(2)).unwrap();
    std::fs::remove_file(&claim_path).unwrap();
    release.send(()).unwrap();

    assert!(admission.drain(Duration::from_secs(2)));
    assert!(observed_cancel.load(Ordering::Acquire));
    let reply: DaemonReply = serde_json::from_slice(&std::fs::read(reply_path).unwrap()).unwrap();
    assert!(matches!(
        reply,
        DaemonReply::Failure { message } if message.starts_with("timed_out:")
    ));
    admission.shutdown();
}

#[test]
fn admission_drain_uses_one_stop_budget_across_all_lanes() {
    let admission = RequestAdmission::new("drain-test", AdmissionTier::Root, one_worker()).unwrap();
    let pools = [
        admission.ingress(),
        admission
            .execution(RequestClass {
                lane: RequestLane::Query,
                verb: "query",
            })
            .1,
        admission
            .execution(RequestClass {
                lane: RequestLane::Maintenance,
                verb: "index",
            })
            .1,
        admission
            .execution(RequestClass {
                lane: RequestLane::Regex,
                verb: "search",
            })
            .1,
        admission
            .execution(RequestClass {
                lane: RequestLane::Semantic,
                verb: "search",
            })
            .1,
    ];
    let (entered, entries) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Arc::new(Mutex::new(released));
    for pool in pools {
        let (entered, released) = (entered.clone(), Arc::clone(&released));
        assert!(
            pool.submit(Box::new(move || {
                entered.send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
            }))
            .is_ok()
        );
    }
    for _ in 0..5 {
        entries.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    let started = Instant::now();
    assert!(!admission.drain(Duration::from_millis(40)));
    assert!(started.elapsed() < Duration::from_millis(100));
    for _ in 0..5 {
        release.send(()).unwrap();
    }
    assert!(admission.drain(Duration::from_secs(1)));
    admission.shutdown();
}
