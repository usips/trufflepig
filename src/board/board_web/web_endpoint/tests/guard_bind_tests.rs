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
fn symlinked_port_file_is_ignored() {
    let (_directory, runtime, _config) = fixture();
    let planted = runtime.join("planted-port");
    fs::write(&planted, "43210").unwrap();
    std::os::unix::fs::symlink(&planted, runtime.join("board-web.port")).unwrap();
    assert_eq!(port_file::read(&runtime), None);
}

#[test]
fn fifo_port_file_does_not_block() {
    let (_directory, runtime, _config) = fixture();
    let fifo = runtime.join("board-web.port");
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: mkfifo on a fresh scratch path; the result is checked.
    let created = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(
        created,
        0,
        "mkfifo failed: {}",
        std::io::Error::last_os_error()
    );
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(port_file::read(&runtime));
    });
    let read = receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("port file read blocked on a FIFO for over five seconds");
    assert_eq!(read, None);
}

#[test]
fn privileged_recorded_port_falls_back() {
    const CHILD: &str = "TRUFFLEPIG_BOARD_WEB_PORT_PRIVILEGED_CHILD";
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: root can bind privileged ports");
        return;
    }
    if std::env::var_os(CHILD).is_none() {
        let directory = crate::board::board_test_support::scratch("web-port-privileged-");
        let runtime = directory.path().join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(CHILD, "1")
            .env("TRUFFLEPIG_BOARD_WEB_TEST_RUNTIME", &runtime)
            .args([
                "board::board_web::web_endpoint::tests::guard_bind_tests::privileged_recorded_port_falls_back",
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
        assert_ne!(fallback, 80);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(&format!(
                "board-serve: recorded port 80 unusable (Permission denied (os error 13)); using {fallback}"
            )),
            "unexpected stderr: {stderr}"
        );
        return;
    }
    let runtime = PathBuf::from(std::env::var_os("TRUFFLEPIG_BOARD_WEB_TEST_RUNTIME").unwrap());
    port_file::record(&runtime, 80).unwrap();
    let listener = bind_listener(&runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let fallback = listener.local_addr().unwrap().port();
    assert_ne!(fallback, 80);
    fs::write(runtime.join("fallback-port"), fallback.to_string()).unwrap();
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
            format!(
                "board-serve: recorded port {taken} unusable \
                 (Address already in use (os error 98)); using {fallback}\n"
            )
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
    port_file::record(&runtime, taken).unwrap();
    let listener = bind_listener(&runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let fallback = listener.local_addr().unwrap().port();
    assert_ne!(fallback, taken);
    fs::write(runtime.join("fallback-port"), fallback.to_string()).unwrap();
}
