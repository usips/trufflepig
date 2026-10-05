use super::*;

#[test]
fn method_refusals_list_the_permitted_methods() {
    let fixture = render_fixture();
    let reply = authed_post(&fixture, "/", &serde_json::json!({}));
    assert!(reply.starts_with("HTTP/1.1 405 "), "{reply}");
    assert!(reply.contains("Allow: GET\r\n"), "{reply}");
    let reply = render_get(&fixture, "/api/v1/challenge");
    assert!(reply.starts_with("HTTP/1.1 405 "), "{reply}");
    assert!(reply.contains("Allow: POST\r\n"), "{reply}");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    write!(
        client,
        "HEAD / HTTP/1.1\r\nHost: {}\r\n\r\n",
        fixture.authority
    )
    .unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    handle(server, Instant::now(), &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 405 "), "{reply}");
    assert!(reply.contains("Allow: GET, POST\r\n"), "{reply}");
}

#[test]
fn private_routes_reject_wrong_methods_with_allow() {
    let fixture = render_fixture();
    for (method, path, allow) in [
        ("POST", "/api/v1/events", "GET"),
        ("GET", "/api/v1/board", "POST"),
        ("GET", "/api/v1/ingest", "POST"),
        ("POST", "/api/v1/render/plan/P1", "GET"),
        ("POST", "/api/v1/render/diff/P1@1..2", "GET"),
        ("POST", "/api/v1/render/proposal/E1", "GET"),
    ] {
        let reply = match method {
            "GET" => render_get(&fixture, path),
            _ => authed_post(&fixture, path, &serde_json::json!({"api": BOARD_API})),
        };
        assert!(
            reply.starts_with("HTTP/1.1 405 "),
            "{method} {path}: {reply}"
        );
        assert!(
            reply.contains(&format!("Allow: {allow}\r\n")),
            "{method} {path}: {reply}"
        );
    }
}
