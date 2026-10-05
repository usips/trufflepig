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
    let accepted_at = Instant::now();
    let mut dripper = client.try_clone().unwrap();
    let producer = std::thread::spawn(move || {
        dripper.write_all(b"G").unwrap();
        std::thread::sleep(Duration::from_secs(3));
        dripper.write_all(b"E").unwrap();
    });
    handle(server, accepted_at, &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    producer.join().unwrap();
    assert!(reply.starts_with("HTTP/1.1 408 "), "{reply}");
    assert!(
        accepted_at.elapsed() < Duration::from_secs(7),
        "a second byte must not restart the five-second hold: {:?}",
        accepted_at.elapsed()
    );
}
