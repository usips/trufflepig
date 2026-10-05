use super::*;

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
                "board::board_web::web_endpoint::tests::guard_bind_tests::taken_persisted_port_falls_back_with_a_one_line_notice",
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
