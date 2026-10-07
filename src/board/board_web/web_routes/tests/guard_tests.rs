use super::*;

#[test]
fn misdirected_render_requests_carry_a_421_code() {
    let fixture = render_fixture();
    for path in [
        "/api/v1/render/plan/P1",
        "/api/v1/render/diff/P1@1..2",
        "/api/v1/render/proposal/E1",
    ] {
        let reply = wrong_host_get(&fixture, path);
        assert!(reply.starts_with("HTTP/1.1 421 "), "{path}: {reply}");
        assert!(
            reply.contains("\"code\":\"misdirected_request\""),
            "{path}: {reply}"
        );
    }
}

#[test]
fn slow_pre_auth_drip_is_cut_at_five_seconds_total_from_accept() {
    let fixture = render_fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    // Injected clock: the accept time sits most of the five-second hold in
    // the past, so only the remaining window costs real time. The drip's
    // second byte lands inside that window and must not restart it.
    const REMAINING: Duration = Duration::from_millis(1500);
    const DRIP_GAP: Duration = Duration::from_millis(900);
    const SCHEDULER_SLACK: Duration = Duration::from_millis(750);
    let accepted_at = Instant::now() - (http_wire::REQUEST_TIMEOUT - REMAINING);
    let mut dripper = client.try_clone().unwrap();
    let producer = std::thread::spawn(move || {
        dripper.write_all(b"G").unwrap();
        std::thread::sleep(DRIP_GAP);
        // The second byte lands inside the remaining window; it must not
        // restart the hold, so the cut still comes at five seconds total.
        let _ = dripper.write_all(b"E");
    });
    handle(server, accepted_at, &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    producer.join().unwrap();
    assert!(reply.starts_with("HTTP/1.1 408 "), "{reply}");
    // The cut lands at the deadline; the refusal drain adds its own bounded
    // wait before the connection closes. Scheduler slack stays below the drip
    // gap, so restarting the remaining hold after the second byte still fails.
    assert!(
        accepted_at.elapsed()
            < http_wire::REQUEST_TIMEOUT + http_wire::DRAIN_TIMEOUT + SCHEDULER_SLACK,
        "a second byte must not restart the five-second hold: {:?}",
        accepted_at.elapsed()
    );
}
