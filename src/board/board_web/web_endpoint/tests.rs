use super::*;
use std::{net::TcpListener, os::unix::fs::PermissionsExt, thread};

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
    let token = BoardWebToken::load_at(&runtime.join("board-web.token")).unwrap();
    let guard = WebGuard::with_token(address, token).unwrap();
    publish(runtime, address, &config.db_path).unwrap();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let request = http_wire::read_request(&mut socket, Instant::now()).unwrap();
        guard.authorize(&request, true, true).unwrap();
        assert_eq!(request.path(), "/api/v1/board");
        let value: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(value["api"], BOARD_API);
        assert_eq!(value["op"]["op"], "feed");
        assert_eq!(value["op"]["limit"], 1);
        let body = serde_json::to_vec(&serde_json::json!({"api":reply_api,
            "result":{"result":"feed","data":{"events":[]}}}))
        .unwrap();
        http_wire::send_response(&mut socket, 200, "application/json", &body, true).unwrap();
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
    BoardWebToken::load_at(&runtime.join("board-web.token")).unwrap();
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
    BoardWebToken::load_at(&runtime.join("board-web.token")).unwrap();
    assert!(
        link_at(&runtime, &config, None)
            .unwrap_err()
            .to_string()
            .contains("invalid web listener address")
    );
}
