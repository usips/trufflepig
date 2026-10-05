use super::*;
use crate::board::board_web::web_guard::{ChallengeNonce, RouteAccess};
use std::{
    io::Read,
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

#[test]
fn web_link_uses_actual_ephemeral_listener_and_bootstrap_reference() {
    let (_directory, runtime, config) = fixture();
    let (address, worker) = listening_fixture(&runtime, &config, BOARD_API);
    let url = link_at(&runtime, &config, Some(BoardRef::parse("P7@2").unwrap())).unwrap();
    worker.join().unwrap();
    assert!(url.starts_with(&format!("http://{address}/?ref=P7@2#token=")));
    assert_eq!(url.split_once("#token=").unwrap().1.len(), 64);
    assert_eq!(
        fs::metadata(runtime.join(ENDPOINT_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    assert!(
        !fs::read_to_string(runtime.join(ENDPOINT_FILE))
            .unwrap()
            .contains("token")
    );
}

#[test]
fn web_link_refuses_stale_endpoint_and_database_mismatch() {
    let (_directory, runtime, config) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    publish(&runtime, address, &config.db_path).unwrap();
    BoardWebToken::rotate_at(&runtime.join("board-web.token")).unwrap();
    drop(listener);
    let stale = link_at(&runtime, &config, None).unwrap_err();
    assert!(
        stale
            .to_string()
            .contains(&format!("no verified listener at http://{address}"))
    );
    let other = BoardConfig::for_database(runtime.join("another.sqlite3"));
    let error = link_at(&runtime, &other, None).unwrap_err();
    assert!(error.to_string().contains("database differs"));
}

#[test]
fn missing_web_endpoint_reports_not_listening_without_creating_token() {
    let (_directory, runtime, config) = fixture();
    let error = link_at(&runtime, &config, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("board web is not listening; run board-serve")
    );
    assert!(!runtime.join("board-web.token").exists());
}

#[test]
fn web_link_refuses_answered_api_mismatch() {
    let (_directory, runtime, config) = fixture();
    let (_address, worker) = listening_fixture(&runtime, &config, BOARD_API + 1);
    assert!(link_at(&runtime, &config, None).is_err());
    worker.join().unwrap();
}

#[test]
fn web_endpoint_rejects_unsafe_files_and_external_address() {
    let (_directory, runtime, config) = fixture();
    let address = "127.0.0.1:7341".parse().unwrap();
    publish(&runtime, address, &config.db_path).unwrap();
    let destination = runtime.join(ENDPOINT_FILE);
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(link_at(&runtime, &config, None).is_err());
    assert!(publish(&runtime, address, &config.db_path).is_err());
    fs::remove_file(&destination).unwrap();
    let other = runtime.join("other.json");
    fs::write(&other, "unchanged").unwrap();
    std::os::unix::fs::symlink(&other, &destination).unwrap();
    assert!(publish(&runtime, address, &config.db_path).is_err());
    assert_eq!(fs::read_to_string(&other).unwrap(), "unchanged");
    fs::remove_file(destination).unwrap();
    publish(&runtime, "192.0.2.1:7341".parse().unwrap(), &config.db_path).unwrap();
    BoardWebToken::rotate_at(&runtime.join("board-web.token")).unwrap();
    assert!(
        link_at(&runtime, &config, None)
            .unwrap_err()
            .to_string()
            .contains("invalid web listener address")
    );
}

/// Read one raw HTTP request (headers plus Content-Length body) for assertions.
fn read_raw_request(socket: &mut TcpStream) -> Vec<u8> {
    let mut raw = Vec::with_capacity(1024);
    let mut chunk = [0; 1024];
    let header_end = loop {
        let count = socket.read(&mut chunk).unwrap();
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
        let count = socket.read(&mut chunk).unwrap();
        assert!(count > 0, "probe closed before sending its body");
        raw.extend_from_slice(&chunk[..count]);
    }
    raw
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

#[test]
fn endpoint_guard_removes_the_published_descriptor_on_drop() {
    let (_directory, runtime, config) = fixture();
    let address = "127.0.0.1:7341".parse().unwrap();
    publish(&runtime, address, &config.db_path).unwrap();
    let descriptor = runtime.join(ENDPOINT_FILE);
    assert!(descriptor.exists());
    drop(EndpointGuard::arm(&runtime, address));
    assert!(!descriptor.exists());
}

#[test]
fn endpoint_guard_keeps_a_foreign_descriptor() {
    let (_directory, runtime, config) = fixture();
    let ours: SocketAddr = "127.0.0.1:7341".parse().unwrap();
    let foreign: SocketAddr = "127.0.0.1:7342".parse().unwrap();
    publish(&runtime, ours, &config.db_path).unwrap();
    let guard = EndpointGuard::arm(&runtime, ours);
    publish(&runtime, foreign, &config.db_path).unwrap();
    drop(guard);
    let bytes = fs::read(runtime.join(ENDPOINT_FILE)).unwrap();
    let endpoint: WebEndpoint = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(endpoint.address, foreign);
}

#[test]
fn clean_exit_keeps_the_bound_port_for_the_next_start() {
    let (_directory, runtime, config) = fixture();
    let first = bind_listener(&runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let address = first.local_addr().unwrap();
    publish(&runtime, address, &config.db_path).unwrap();
    let guard = EndpointGuard::arm(&runtime, address);
    drop(guard);
    assert!(
        !runtime.join(ENDPOINT_FILE).exists(),
        "a clean exit removes the published descriptor"
    );
    drop(first);
    let persisted = runtime.join("board-web.port");
    assert_eq!(
        fs::read_to_string(&persisted).unwrap(),
        address.port().to_string(),
        "the bound port survives the clean exit"
    );
    assert_eq!(
        fs::metadata(&persisted).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    let second = bind_listener(&runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    assert_eq!(second.local_addr().unwrap().port(), address.port());
}

#[test]
fn taken_persisted_port_falls_back_with_a_one_line_notice() {
    const CHILD: &str = "TRUFFLEPIG_BOARD_WEB_PORT_FALLBACK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let directory = crate::board::board_test_support::scratch("web-port-fallback-");
        let runtime = directory.path().join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let holder = TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = holder.local_addr().unwrap().port();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(CHILD, "1")
            .env("TRUFFLEPIG_BOARD_WEB_TEST_RUNTIME", &runtime)
            .env("TRUFFLEPIG_BOARD_WEB_TEST_TAKEN", taken.to_string())
            .args([
                "board::board_web::web_endpoint::tests::taken_persisted_port_falls_back_with_a_one_line_notice",
                "--exact",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let fallback: u16 = fs::read_to_string(runtime.join("fallback-port"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_ne!(fallback, taken);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(
            stderr,
            format!("board-serve: port {taken} in use; using {fallback}\n")
        );
        drop(holder);
        return;
    }
    let runtime = PathBuf::from(std::env::var_os("TRUFFLEPIG_BOARD_WEB_TEST_RUNTIME").unwrap());
    let taken: u16 = std::env::var_os("TRUFFLEPIG_BOARD_WEB_TEST_TAKEN")
        .unwrap()
        .into_string()
        .unwrap()
        .parse()
        .unwrap();
    fs::write(runtime.join("board-web.port"), taken.to_string()).unwrap();
    let listener = bind_listener(&runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let fallback = listener.local_addr().unwrap().port();
    assert_ne!(fallback, taken);
    fs::write(runtime.join("fallback-port"), fallback.to_string()).unwrap();
}
