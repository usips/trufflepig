use super::*;

#[test]
fn ingest_posts_answer_202_without_waiting_for_the_relay() {
    let fixture = render_fixture();
    for _ in 0..2 {
        let reply = authed_post(
            &fixture,
            "/api/v1/ingest",
            &serde_json::json!({"api": BOARD_API}),
        );
        assert!(reply.starts_with("HTTP/1.1 202 "), "{reply}");
        assert!(reply.contains("\"queued\""), "{reply}");
    }
}

#[test]
fn ingest_202_and_receipt_carry_the_same_flight_ticket() {
    let fixture = render_fixture_with(Arc::new(|_, _, _| {
        Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    }));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut subscriber = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    subscriber
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    fixture
        .state
        .streams
        .spawn(
            server,
            StreamRequest::parse(None, None, None).unwrap(),
            fixture.state.streams.reserve().unwrap(),
        )
        .unwrap();
    read_until(&mut subscriber, "\r\n\r\n");
    let reply = authed_post(
        &fixture,
        "/api/v1/ingest",
        &serde_json::json!({"api": BOARD_API}),
    );
    assert!(reply.starts_with("HTTP/1.1 202 "), "{reply}");
    let body: serde_json::Value =
        serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["ingest"], "queued");
    let ticket = body["ticket"]
        .as_str()
        .expect("the 202 carries the flight ticket");
    // The fixture has no router, so the relay publishes its failure receipt.
    let frame = read_until(&mut subscriber, "}\n\n");
    assert!(frame.contains("event: ingest\n"), "{frame}");
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let receipt: serde_json::Value = serde_json::from_str(data).unwrap();
    assert_eq!(receipt["ticket"], ticket);
}

#[test]
fn rerun_after_a_25s_first_scan_still_succeeds() {
    let fixture = render_fixture_with(Arc::new(|_, _, _| {
        Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    }));
    let runtime = fixture._directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let socket = runtime.join(crate::daemon::SOCKET_NAME);
    let board_db = fixture.config.db_path.to_string_lossy().into_owned();
    // Fake router: instant status probes, a 25 s first scan, a 10 s rerun.
    // Under one inherited 30 s deadline the rerun's 10 s of work cannot fit
    // the ~5 s left; only a fresh per-scan deadline lets it succeed.
    let router = std::thread::spawn(move || {
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut ingests = 0;
        // Two scans of probe plus ingest, served in relay order.
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            let args = read_daemon_args(&mut stream);
            if args == ["system", "status"] {
                write_daemon_reply(
                    &mut stream,
                    &serde_json::json!({
                        "status": "ok", "board_api": BOARD_API, "board_db": board_db,
                    })
                    .to_string(),
                );
            } else {
                assert_eq!(
                    args,
                    ["--json", "board", "ingest"],
                    "unexpected router call: {args:?}"
                );
                ingests += 1;
                std::thread::sleep(Duration::from_secs(if ingests == 1 { 25 } else { 10 }));
                write_daemon_reply(
                    &mut stream,
                    &serde_json::json!({"api": BOARD_API, "inserted": 1}).to_string(),
                );
            }
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut subscriber = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    subscriber
        .set_read_timeout(Some(Duration::from_secs(90)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    fixture
        .state
        .streams
        .spawn(
            server,
            StreamRequest::parse(None, None, None).unwrap(),
            fixture.state.streams.reserve().unwrap(),
        )
        .unwrap();
    let mut transcript = read_until(&mut subscriber, "\r\n\r\n");
    let post = || {
        let reply = authed_post(
            &fixture,
            "/api/v1/ingest",
            &serde_json::json!({"api": BOARD_API}),
        );
        assert!(reply.starts_with("HTTP/1.1 202 "), "{reply}");
        let body: serde_json::Value =
            serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["ingest"], "queued");
        body["ticket"].as_str().unwrap().to_owned()
    };
    let leader = post();
    // The first scan sleeps 25 s, so this POST always lands mid-scan.
    let joiner = post();
    assert_ne!(
        joiner, leader,
        "a mid-scan POST holds the next ticket, not the running scan's"
    );
    // Drain both ingest frames; each frame carries exactly one data line,
    // and only ingest receipts carry a flight ticket.
    let mut receipts = Vec::new();
    while receipts.len() < 2 {
        transcript.push_str(&read_until(&mut subscriber, "event: ingest\n"));
        receipts = transcript
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
            .filter(|receipt| receipt.get("ticket").is_some())
            .collect::<Vec<_>>();
    }
    assert_eq!(receipts[0]["ticket"], leader);
    assert_eq!(receipts[1]["ticket"], joiner);
    for receipt in &receipts {
        assert!(
            receipt.get("error").is_none(),
            "the rerun still succeeds on its own deadline: {receipt}"
        );
    }
    router.join().unwrap();
}
