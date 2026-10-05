use super::*;

#[test]
fn challenge_route_answers_unauthenticated_with_a_verifiable_proof() {
    let fixture = render_fixture();
    let nonce = crate::board::board_web::web_guard::ChallengeNonce::generate().unwrap();
    let reply = challenge_post(
        &fixture,
        &serde_json::json!({"api": BOARD_API, "nonce": nonce.to_hex()}),
    );
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
    assert!(!reply.contains(fixture.token.expose()), "{reply}");
    let json: serde_json::Value =
        serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(json["api"], BOARD_API);
    let proof = json["proof"].as_str().unwrap();
    assert!(fixture.state.guard.challenge_matches(&nonce, proof));
    let other = BoardWebToken::rotate_at(&fixture._directory.path().join("other.token")).unwrap();
    let address: std::net::SocketAddr = fixture.authority.parse().unwrap();
    let foreign = WebGuard::with_token(address, other).unwrap();
    assert!(!foreign.challenge_matches(&nonce, proof));
}

#[test]
fn challenge_route_keeps_method_and_origin_checks_without_a_token() {
    let fixture = render_fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    write!(
        client,
        "GET /api/v1/challenge HTTP/1.1\r\nHost: {}\r\n\r\n",
        fixture.authority
    )
    .unwrap();
    handle(server, Instant::now(), &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 405 "), "{reply}");
    let reply = challenge_post(
        &fixture,
        &serde_json::json!({"api": BOARD_API + 1, "nonce": "0".repeat(64)}),
    );
    assert!(reply.starts_with("HTTP/1.1 400 "), "{reply}");
    assert!(reply.contains("\"board_api_mismatch\""), "{reply}");
}
