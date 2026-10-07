mod guard_bind_tests;
mod link_tests;
mod probe_tests;

use super::*;
use crate::board::board_web::web_guard::{ChallengeNonce, RouteAccess};
use std::{
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    thread,
};

fn fixture() -> (tempfile::TempDir, PathBuf, BoardConfig) {
    let directory = crate::board::board_test_support::scratch("web-endpoint-");
    let runtime = directory.path().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    (directory, runtime, config)
}

fn listening_fixture(
    runtime: &Path,
    config: &BoardConfig,
    reply_api: u32,
) -> (SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let token = BoardWebToken::rotate_at(&runtime.join("board-web.token")).unwrap();
    let guard = WebGuard::with_token(address, token).unwrap();
    publish(runtime, address, &config.db_path).unwrap();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let request = http_wire::read_request(&mut socket, Instant::now()).unwrap();
        guard
            .authorize(&request, RouteAccess::Challenge, true)
            .unwrap();
        assert_eq!(request.path(), "/api/v1/challenge");
        assert!(request.header("x-board-token").is_none());
        let value: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(value["api"], BOARD_API);
        let nonce = ChallengeNonce::from_hex(value["nonce"].as_str().unwrap()).unwrap();
        let body = serde_json::to_vec(
            &serde_json::json!({"api":reply_api,"proof":guard.challenge_proof(&nonce)}),
        )
        .unwrap();
        http_wire::send_response(&mut socket, 200, "application/json", &body).unwrap();
    });
    (address, worker)
}

/// Read one raw HTTP request (headers plus Content-Length body) for assertions.
fn read_raw_request(socket: &mut TcpStream) -> Vec<u8> {
    let mut raw = Vec::with_capacity(1024);
    let mut chunk = [0; 1024];
    let header_end = loop {
        let count =
            crate::board::board_test_support::read_ignoring_interrupts(socket, &mut chunk).unwrap();
        assert!(count > 0, "probe closed before finishing its request");
        raw.extend_from_slice(&chunk[..count]);
        if let Some(end) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = std::str::from_utf8(&raw[..header_end]).unwrap();
    let length: usize = headers
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse().unwrap())
        .expect("probe request carries a content length");
    while raw.len() < header_end + length {
        let count =
            crate::board::board_test_support::read_ignoring_interrupts(socket, &mut chunk).unwrap();
        assert!(count > 0, "probe closed before sending its body");
        raw.extend_from_slice(&chunk[..count]);
    }
    raw
}
