mod authorize_tests;
mod bind_challenge_tests;
mod host_origin_tests;
mod token_security;

use super::*;
use std::{collections::BTreeMap, io::Write, net::TcpStream, time::Instant};

fn fixture() -> (tempfile::TempDir, WebGuard) {
    let directory = crate::board::board_test_support::scratch("web-guard-");
    let token = BoardWebToken::rotate_at(&directory.path().join("board-web.token")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let guard = WebGuard::with_token(listener.local_addr().unwrap(), token).unwrap();
    (directory, guard)
}

fn request(guard: &WebGuard, method: HttpMethod, authenticated: bool) -> HttpRequest {
    let mut headers = BTreeMap::new();
    headers.insert("host".to_owned(), guard.authority().to_owned());
    if authenticated {
        headers.insert("x-board-token".to_owned(), guard.token.expose().to_owned());
    }
    if method == HttpMethod::Post {
        headers.insert("origin".to_owned(), guard.origin().to_owned());
        headers.insert("content-type".to_owned(), "application/json".to_owned());
    }
    HttpRequest {
        method,
        target: "/api/v1/board".to_owned(),
        headers,
        body: Vec::new(),
    }
}
