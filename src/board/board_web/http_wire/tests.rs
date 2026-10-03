mod wire_deadlines;

use super::*;
use std::{
    io::Write,
    net::{Shutdown, TcpListener},
};

fn tcp_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    (server, client)
}

fn parse(bytes: &[u8]) -> Result<HttpRequest, HttpError> {
    let (mut server, mut client) = tcp_pair();
    client.write_all(bytes).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    read_request(&mut server, Instant::now())
}

fn rejected(bytes: &[u8], status: u16) {
    assert_eq!(parse(bytes).unwrap_err().status, status, "{bytes:?}");
}

#[test]
fn accepts_origin_form_and_exact_binary_body() {
    let request = parse(b"POST /api/v1/board?cursor=4 HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nContent-Length: 4\r\nContent-Type: application/json\r\n\r\n\0\xff\r\n").unwrap();
    assert_eq!(request.method, HttpMethod::Post);
    assert_eq!(request.target, "/api/v1/board?cursor=4");
    assert_eq!(request.path(), "/api/v1/board");
    assert_eq!(request.header("HOST"), Some("127.0.0.1:1234"));
    assert_eq!(request.body, b"\0\xff\r\n");
}

#[test]
fn rejects_buffered_security_and_framing_attacks() {
    for bytes in [
        &b"GET / HTTP/1.1\nHost: local\n\n"[..],
        &b"GET / HTTP/1.1\rHost: local\r\n\r\n"[..],
        &b"GET  / HTTP/1.1\r\nHost: local\r\n\r\n"[..],
        &b"GE\tT / HTTP/1.1\r\nHost: local\r\n\r\n"[..],
        &b"GET http://local/ HTTP/1.1\r\nHost: local\r\n\r\n"[..],
        &b"GET //local/ HTTP/1.1\r\nHost: local\r\n\r\n"[..],
        &b"GET /#secret HTTP/1.1\r\nHost: local\r\n\r\n"[..],
        &b"GET /\\foo HTTP/1.1\r\nHost: local\r\n\r\n"[..],
        &b"GET / HTTP/1.0\r\nHost: local\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\nHost : local\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\nHost:\tlocal\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\nHost: local\r\n folded: value\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\nHost: local\r\nX-Test: a\0b\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\nHost: local\r\nX-Test: a\x7fb\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: identity\r\n\r\n"[..],
        &b"POST / HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\nHost: local\r\nUpgrade: websocket\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\nHost: local\r\nConnection: keep-alive, Upgrade\r\n\r\n"[..],
        &b"GET / HTTP/1.1\r\nHost: local\r\nContent-Length: 1\r\n\r\na"[..],
        &b"GET / HTTP/1.1\r\nHost: local\r\n\r\nbody"[..],
        &b"GET / HTTP/1.1\r\nHost: local\r\n\r\nGET /private HTTP/1.1\r\nHost: local\r\n\r\n"[..],
        &b"POST / HTTP/1.1\r\nHost: local\r\nContent-Length: 2\r\n\r\n{}GET / HTTP/1.1\r\nHost: local\r\n\r\n"[..],
    ] {
        rejected(bytes, 400);
    }
    rejected(
        b"POST / HTTP/1.1\r\nHost: local\r\nExpect: 100-continue\r\nContent-Length: 0\r\n\r\n",
        417,
    );
    rejected(b"HEAD / HTTP/1.1\r\nHost: local\r\n\r\n", 405);
}

#[test]
fn rejects_case_insensitive_duplicate_security_headers() {
    for duplicate in [
        "hOsT: local\r\n",
        "Content-Length: 0\r\ncOnTeNt-LeNgTh: 0\r\n",
        "Origin: http://local\r\noRiGiN: http://local\r\n",
        "X-Board-Token: a\r\nx-board-token: a\r\n",
        "Content-Type: application/json\r\ncontent-type: application/json\r\n",
    ] {
        let bytes = format!("GET / HTTP/1.1\r\nHost: local\r\n{duplicate}\r\n");
        rejected(bytes.as_bytes(), 400);
    }
}

#[test]
fn checks_content_length_decimal_overflow_and_limits() {
    for length in ["", "+1", "-1", "1 0", "1,1", "0x1", "184467440737095516160"] {
        rejected(
            format!("POST / HTTP/1.1\r\nHost: local\r\nContent-Length: {length}\r\n\r\n")
                .as_bytes(),
            400,
        );
    }
    rejected(b"POST / HTTP/1.1\r\nHost: local\r\n\r\n", 411);
    rejected(
        b"POST / HTTP/1.1\r\nHost: local\r\nContent-Length: 65537\r\n\r\n",
        413,
    );
    let mut request =
        format!("POST / HTTP/1.1\r\nHost: local\r\nContent-Length: {BODY_LIMIT}\r\n\r\n")
            .into_bytes();
    request.resize(request.len() + BODY_LIMIT, b'a');
    assert_eq!(parse(&request).unwrap().body.len(), BODY_LIMIT);
}

#[test]
fn checks_total_header_limit_including_request_line() {
    let prefix = b"GET / HTTP/1.1\r\nHost: local\r\nX-Padding: ";
    let mut exact = prefix.to_vec();
    exact.resize(HEADER_LIMIT - 4, b'a');
    exact.extend_from_slice(b"\r\n\r\n");
    assert!(parse(&exact).is_ok());
    let mut oversized = prefix.to_vec();
    oversized.resize(HEADER_LIMIT - 3, b'a');
    oversized.extend_from_slice(b"\r\n\r\n");
    rejected(&oversized, 431);
}

#[test]
fn eof_never_completes_a_partial_request() {
    rejected(b"GET / HTTP/1.1\r\nHost: local\r\n", 400);
    rejected(
        b"POST / HTTP/1.1\r\nHost: local\r\nContent-Length: 2\r\n\r\n{",
        400,
    );
}

#[test]
fn private_response_has_safe_framing() {
    let (mut server, mut client) = tcp_pair();
    send_response(&mut server, 200, "application/json", b"{}", true).unwrap();
    drop(server);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.contains("Content-Length: 2\r\n"));
    assert!(reply.contains("Connection: close\r\n"));
    assert!(reply.contains("Cache-Control: no-store\r\n"));
    assert!(reply.contains("frame-ancestors 'none'"));
    assert!(!reply.contains("Access-Control-Allow"));
    assert!(reply.ends_with("\r\n\r\n{}"));
}

#[test]
fn unavailable_response_has_typed_retry_after_and_private_headers() {
    let (mut server, mut client) = tcp_pair();
    send_unavailable(&mut server, 5, b"{}").unwrap();
    drop(server);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
    assert!(reply.contains("Retry-After: 5\r\n"));
    assert!(reply.contains("Cache-Control: no-store\r\n"));
    assert!(reply.contains("Content-Length: 2\r\n"));
    assert!(reply.contains("Connection: close\r\n"));
    let precomputed = unavailable_response(5, b"{}");
    assert_eq!(reply.as_bytes(), precomputed.as_slice());
}
