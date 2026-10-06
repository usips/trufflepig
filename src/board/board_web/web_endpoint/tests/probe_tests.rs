use super::*;

#[test]
fn probe_retries_an_interrupted_read() {
    // A signal may interrupt the probe's blocking reply read; the probe
    // retries instead of failing, because an interrupted read delivered
    // no bytes and lost nothing.
    struct FlakyReply {
        interrupted: bool,
    }
    impl std::io::Read for FlakyReply {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupted {
                bytes[..3].copy_from_slice(b"200");
                return Ok(3);
            }
            self.interrupted = true;
            Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "EINTR",
            ))
        }
    }
    let mut reply = FlakyReply { interrupted: false };
    let mut chunk = [0; 8];
    let count = http_wire::read_ignoring_interrupts(&mut reply, &mut chunk).unwrap();
    assert_eq!(&chunk[..count], b"200");
    assert!(
        reply.interrupted,
        "the first read must have been interrupted"
    );
}

#[test]
fn probe_proves_ownership_without_sending_the_token() {
    let (_directory, runtime, config) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let token = BoardWebToken::rotate_at(&runtime.join("board-web.token")).unwrap();
    let guard = WebGuard::with_token(address, token.clone()).unwrap();
    publish(&runtime, address, &config.db_path).unwrap();
    let exposed = token.expose().to_owned();
    let answering = WebGuard::with_token(address, token.clone()).unwrap();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let raw = read_raw_request(&mut socket);
        let text = String::from_utf8(raw).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let nonce = ChallengeNonce::from_hex(body["nonce"].as_str().unwrap()).unwrap();
        let reply = serde_json::to_vec(
            &serde_json::json!({"api":BOARD_API,"proof":answering.challenge_proof(&nonce)}),
        )
        .unwrap();
        http_wire::send_response(&mut socket, 200, "application/json", &reply).unwrap();
        text
    });
    endpoint_probe::probe(address, &guard, Instant::now() + http_wire::REQUEST_TIMEOUT).unwrap();
    let raw = worker.join().unwrap();
    assert!(
        !raw.contains(&exposed),
        "probe must never send the token on the wire: {raw}"
    );
    assert!(
        !raw.to_lowercase().contains("x-board-token"),
        "probe must not carry a token header: {raw}"
    );
}

#[test]
fn probe_refuses_a_listener_proving_with_the_wrong_token() {
    let (_directory, runtime, config) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let token = BoardWebToken::rotate_at(&runtime.join("board-web.token")).unwrap();
    let guard = WebGuard::with_token(address, token).unwrap();
    publish(&runtime, address, &config.db_path).unwrap();
    let thief_token = BoardWebToken::rotate_at(&runtime.join("thief.token")).unwrap();
    let thief = WebGuard::with_token(address, thief_token).unwrap();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let request = http_wire::read_request(&mut socket, Instant::now()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let nonce = ChallengeNonce::from_hex(value["nonce"].as_str().unwrap()).unwrap();
        let reply = serde_json::to_vec(
            &serde_json::json!({"api":BOARD_API,"proof":thief.challenge_proof(&nonce)}),
        )
        .unwrap();
        http_wire::send_response(&mut socket, 200, "application/json", &reply).unwrap();
    });
    let error = endpoint_probe::probe(address, &guard, Instant::now() + http_wire::REQUEST_TIMEOUT)
        .unwrap_err();
    worker.join().unwrap();
    assert!(
        error.to_string().contains("failed the ownership challenge"),
        "{error}"
    );
}
