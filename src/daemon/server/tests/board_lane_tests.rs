use super::*;

/// Router lanes come from `lane_size`; the query pool is irrelevant here.
fn router(handler: TestDaemon) -> Arc<Served> {
    Arc::new(Served::start_tier(
        AdmissionTier::Router,
        false,
        WIDE,
        handler,
    ))
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
