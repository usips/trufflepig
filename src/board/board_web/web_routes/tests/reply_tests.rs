use super::*;

#[test]
fn unavailable_board_errors_include_retry_after_and_security_headers() {
    for code in [
        BoardErrorCode::DaemonBusy,
        BoardErrorCode::BoardUnavailable,
        BoardErrorCode::DatabaseLocked,
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        send_board_error(&mut server, BoardError::new(code, "unavailable"));
        server.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 503 "));
        assert!(reply.contains("Retry-After: 1\r\n"));
        assert!(reply.contains("Cache-Control: no-store\r\n"));
        assert!(reply.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(reply.contains("Content-Security-Policy:"));
    }
}

#[test]
fn early_errors_send_their_reply_before_closing_on_unread_input() {
    let fixture = render_fixture();
    let mut oversized_headers = b"GET / HTTP/1.1\r\nHost: x\r\nX-Pad: ".to_vec();
    oversized_headers.resize(64 * 1024, b'a');
    let mut oversized_body =
        b"POST /api/v1/board HTTP/1.1\r\nHost: x\r\nContent-Length: 999999\r\n\r\n".to_vec();
    oversized_body.resize(oversized_body.len() + 4096, b'b');
    for (bytes, status) in [(oversized_headers, "431"), (oversized_body, "413")] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        client.write_all(&bytes).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        handle(server, Instant::now(), &fixture.state);
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        let reply = String::from_utf8(reply).unwrap();
        assert!(reply.starts_with(&format!("HTTP/1.1 {status} ")), "{reply}");
        assert!(reply.ends_with("}}"), "{reply}");
    }
}

/// Polls until `needle` sits unread in the socket's receive buffer; the 5 s
/// deadline turns a stuck producer into a failure instead of a hang. Peeking
/// never consumes, so the bytes stay queued for the code under test.
fn peek_until(socket: &mut TcpStream, needle: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buffered = vec![0; 64 * 1024];
    socket.set_nonblocking(true).unwrap();
    loop {
        let arrived = match socket.peek(&mut buffered) {
            Ok(count) => buffered[..count]
                .windows(needle.len())
                .any(|window| window == needle),
            Err(_) => false,
        };
        if arrived {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {:?}",
            String::from_utf8_lossy(needle)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    socket.set_nonblocking(false).unwrap();
}

#[test]
fn queue_full_refusal_delivers_the_busy_reply_before_closing() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    // A mid-request client: bytes queued unread server-side, no FIN yet.
    client
        .write_all(b"GET /api/v1/board HTTP/1.1\r\nHost: x")
        .unwrap();
    // The request must be queued unread server-side before the refusal, and
    // the reply must sit unread client-side before the read, so a reset
    // cannot slip past already-consumed bytes on either side.
    peek_until(&mut server, b"Host: x");
    let busy =
        http_wire::unavailable_response(1, crate::board::board_web::web_serve::QUEUE_FULL_BODY);
    crate::board::board_web::web_serve::refuse_queue_full(server, &busy);
    peek_until(&mut client, b"daemon_busy");
    // A reset may already have destroyed the connection; the body read below
    // is the assertion.
    let _ = client.shutdown(Shutdown::Write);
    let mut reply = Vec::new();
    client.read_to_end(&mut reply).unwrap();
    let reply = String::from_utf8(reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 503 "), "{reply}");
    assert!(reply.contains("daemon_busy"), "{reply}");
    assert!(reply.ends_with("}}"), "{reply}");
}
