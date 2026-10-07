use super::*;
use std::{
    fs::File,
    io,
    process::{Child, Command, Stdio},
    time::Duration,
};

const CHILD_MODE: &str = "TRUFFLEPIG_BOARD_WEB_CLEAN_RESTART_CHILD";
const TEST_NAME: &str = concat!(
    "board::board_web::web_endpoint::tests::guard_bind_tests::",
    "clean_exit_keeps_the_bound_port_for_the_next_start"
);
const SUCCESS_MARKER: &str = "board-web clean restart child: retained exact port";

pub(super) fn check_clean_restart() {
    if std::env::var_os(CHILD_MODE).is_some() {
        assert_clean_restart();
        eprintln!("\n{SUCCESS_MARKER}");
        return;
    }
    let directory = crate::board::board_test_support::scratch("web-clean-restart-child-");
    let log_path = directory.path().join("child.log");
    let output = File::create(&log_path).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .env(CHILD_MODE, "1")
        .args([TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output))
        .spawn()
        .unwrap();
    let mut child = CleanRestartChild(child);
    let expires = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            let log = fs::read_to_string(&log_path).unwrap();
            assert!(
                status.success(),
                "clean restart child failed: {status}\n{log}"
            );
            assert!(
                log.lines().any(|line| line == "running 1 test"),
                "clean restart child must run exactly one test\n{log}"
            );
            assert!(
                log.lines().any(|line| line == SUCCESS_MARKER),
                "clean restart child did not complete the strict rebind\n{log}"
            );
            return;
        }
        assert!(
            Instant::now() < expires,
            "clean restart child exceeded five seconds\n{}",
            fs::read_to_string(&log_path).unwrap()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_clean_restart() {
    let (_directory, runtime, config) = fixture();
    // Opening after exec prevents other harness threads from inheriting this
    // listener; explicit ports outside the ephemeral range avoid bind(0) races.
    let first = bind_outside_ephemeral_range(&runtime);
    let address = first.local_addr().unwrap();
    publish(&runtime, address, &config.db_path).unwrap();
    drop(EndpointGuard::arm(&runtime, address));
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

fn bind_outside_ephemeral_range(runtime: &Path) -> TcpListener {
    let range = fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").unwrap();
    let mut parts = range.split_whitespace();
    let first: u16 = parts.next().unwrap().parse().unwrap();
    let last: u16 = parts.next().unwrap().parse().unwrap();
    assert!(
        first <= last && parts.next().is_none(),
        "invalid ephemeral range: {range}"
    );
    let unprivileged: u16 = fs::read_to_string("/proc/sys/net/ipv4/ip_unprivileged_port_start")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let candidates = (16384..=u16::MAX)
        .chain(1024..16384)
        .filter(|port| *port >= unprivileged.max(1024) && !(first..=last).contains(port))
        .take(64);
    for port in candidates {
        match bind_listener(runtime, SocketAddr::from(([127, 0, 0, 1], port))) {
            Ok(listener) => return listener,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => panic!("initial clean-restart bind failed at port {port}: {error}"),
        }
    }
    panic!("no available unprivileged port outside {first}..={last} after 64 candidates");
}

struct CleanRestartChild(Child);

impl Drop for CleanRestartChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
