use super::*;
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Command, Stdio},
    time::Instant,
};

const CHILD_MODE: &str = "TRUFFLEPIG_BOARD_RETRY_PROBE_CHILD";

/// Reuses the existing test in a child whose router/config paths are isolated.
pub(super) fn run_isolated() -> bool {
    if std::env::var_os(CHILD_MODE).is_some() {
        check_real_retry();
        return true;
    }
    let directory = scratch();
    let log_path = directory.path().join("child.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let module = module_path!().split_once("::").unwrap().1;
    let module = module.strip_suffix("::cli_retry_probe").unwrap();
    let test = format!(
        "{module}::router_api_is_probed_once_before_dispatch_and_mismatch_never_falls_back"
    );
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test, "--nocapture"])
        .env(CHILD_MODE, "1")
        .env("TRUFFLEPIG_SYSTEM_DIR", directory.path().join("runtime"))
        .env("TRUFFLEPIG_SPOOL_DIR", directory.path().join("spool"))
        .env(
            "TRUFFLEPIG_BOARD_DB",
            directory.path().join("board.sqlite3"),
        )
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .env("XDG_DATA_HOME", directory.path().join("data"))
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated CLI retry probe timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = std::fs::read_to_string(log_path).unwrap();
    assert!(status.success(), "{output}");
    assert!(
        output.contains("real CLI retry: one probe, fresh request ids"),
        "{output}"
    );
    false
}

fn check_real_retry() {
    let runtime = PathBuf::from(std::env::var_os("TRUFFLEPIG_SYSTEM_DIR").unwrap());
    let database = PathBuf::from(std::env::var_os("TRUFFLEPIG_BOARD_DB").unwrap());
    assert_eq!(crate::system::dir().as_ref(), Some(&runtime));
    std::fs::create_dir(&runtime).unwrap();
    let listener = UnixListener::bind(runtime.join(crate::daemon::SOCKET_NAME)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let status = serde_json::json!({"board_api": BOARD_API, "board_db": database}).to_string();
    let server = std::thread::spawn(move || serve_attempts(listener, status));
    let args = [
        "--client",
        "codex",
        "--session",
        "retry-probe",
        "board",
        "show",
    ]
    .map(str::to_owned);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = crate::cli::emission::execute(&args, &mut stdout, &mut stderr);
    let requests = server.join().unwrap();
    assert_eq!(exit, 0, "{}", String::from_utf8_lossy(&stderr));
    assert!(String::from_utf8_lossy(&stdout).contains("retry-ok"));
    assert!(stderr.is_empty());
    let probes: Vec<_> = requests
        .iter()
        .filter(|request| request["arguments"]["args"] == serde_json::json!(["system", "status"]))
        .collect();
    let attempts: Vec<_> = requests
        .iter()
        .filter(|request| request["arguments"]["args"] != serde_json::json!(["system", "status"]))
        .collect();
    assert_eq!(
        probes.len(),
        1,
        "outer CLI retry repeated the capability probe"
    );
    assert_eq!(attempts.len(), 2);
    let first = &attempts[0]["arguments"]["context"];
    let second = &attempts[1]["arguments"]["context"];
    assert_ne!(first["request_id"], second["request_id"]);
    assert_eq!(first["session"], second["session"]);
    assert_eq!(first["client"], second["client"]);
    assert!(!database.exists());
    println!("real CLI retry: one probe, fresh request ids");
}

fn serve_attempts(listener: UnixListener, status: String) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut requests = Vec::with_capacity(4);
    let mut attempts = 0;
    while attempts < 2 {
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "CLI never reached the fake router"
                );
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(error) => panic!("fake router accept: {error}"),
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let request = read_request(&mut stream);
        let reply = if request["arguments"]["args"] == serde_json::json!(["system", "status"]) {
            serde_json::json!({"status": "success", "output": status})
        } else {
            attempts += 1;
            if attempts == 1 {
                serde_json::json!({"status": "failure", "message": "daemon_busy: injected contention"})
            } else {
                serde_json::json!({"status": "success", "output": "retry-ok"})
            }
        };
        requests.push(request);
        let bytes = serde_json::to_vec(&reply).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&bytes).unwrap();
    }
    requests
}

fn read_request(stream: &mut UnixStream) -> serde_json::Value {
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let size = u32::from_be_bytes(header) as usize;
    assert!(size <= crate::daemon::MAX_DAEMON_REQUEST_BYTES);
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
