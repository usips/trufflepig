use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::process_fixture::kill_process_group;

const REQUEST_LIMIT: usize = 64 * 1024;
const THREAD_DEADLINE: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct WireRequest {
    pub request_id: String,
    pub args: Vec<String>,
}

pub struct BusyDaemon {
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<WireRequest>>>,
    worker: Option<JoinHandle<()>>,
}

pub struct SystemServer {
    child: Option<Child>,
    process_group: i32,
    socket: std::path::PathBuf,
    stdout: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    stderr: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
}

impl BusyDaemon {
    pub fn busy_reply() -> &'static str {
        r#"{"status":"failure","message":"daemon_busy: pool=router-query queue_depth=64 queue_capacity=64 cause=queue_full retry_after_ms=250 hint=retry_with_bounded_backoff active_verb=show"}"#
    }

    pub fn start(system_dir: &Path, reply: &'static str) -> Self {
        let socket = system_dir.join("daemon.sock");
        assert!(!socket.exists(), "fixture socket already exists");
        let listener = UnixListener::bind(&socket).expect("bind fake busy router");
        listener
            .set_nonblocking(true)
            .expect("make fake router listener nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_requests = Arc::clone(&requests);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + THREAD_DEADLINE;
            while !thread_stop.load(Ordering::Acquire) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((stream, _)) => handle(stream, reply, &thread_requests),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            stop,
            requests,
            worker: Some(worker),
        }
    }

    pub fn requests(&self) -> Vec<WireRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn all_request_ids_are_fresh(&self) -> bool {
        let requests = self.requests();
        let unique = requests
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        unique.len() == requests.len()
    }

    pub fn requests_preserve_root_and_cache(&self, root: &Path, cache: &Path) -> bool {
        self.requests().iter().all(|request| {
            has_option_path(&request.args, "--root", root)
                && has_option_path(&request.args, "--cache", cache)
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join fake busy router");
        }
    }
}

impl SystemServer {
    pub fn start(command: &mut Command, socket: std::path::PathBuf, timeout: Duration) -> Self {
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn isolated system router");
        let process_group = child.id() as i32;
        let stdout = child.stdout.take().expect("piped router stdout");
        let stderr = child.stderr.take().expect("piped router stderr");
        let mut server = Self {
            child: Some(child),
            process_group,
            socket,
            stdout: Some(thread::spawn(move || drain(stdout))),
            stderr: Some(thread::spawn(move || drain(stderr))),
        };
        server.wait_ready(timeout);
        server
    }

    pub fn assert_running(&mut self) {
        assert!(self.child_alive(), "isolated system router child exited");
        assert!(
            UnixStream::connect(&self.socket).is_ok(),
            "isolated system router socket stopped accepting connections"
        );
        assert!(
            self.child_alive(),
            "isolated system router exited after probe"
        );
    }

    fn wait_ready(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if UnixStream::connect(&self.socket).is_ok() {
                if self.child_alive() {
                    return;
                }
                self.fail_start("router exited as its socket became reachable");
            }
            if !self.child_alive() {
                self.fail_start("router exited before binding its isolated socket");
            }
            if Instant::now() >= deadline {
                self.fail_start("router readiness exceeded its deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn child_alive(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    fn fail_start(&mut self, reason: &str) -> ! {
        kill_process_group(self.process_group, libc::SIGKILL);
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
            self.child.take();
        }
        let (stdout, stderr) = self.take_output();
        panic!("{reason}; router stdout: {stdout}; stderr: {stderr}");
    }

    fn shutdown(&mut self) {
        if self.child.is_none() {
            kill_process_group(self.process_group, libc::SIGKILL);
            let _ = self.take_output();
            return;
        }
        request_stop(&self.socket);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let exited = self
                .child
                .as_mut()
                .is_none_or(|child| child.try_wait().ok().flatten().is_some());
            if exited {
                self.child.take();
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Stop lets the router drain normally; kill any remaining router-group
        // members after the bounded grace period.
        kill_process_group(self.process_group, libc::SIGKILL);
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
            self.child.take();
        }
        let _ = self.take_output();
    }

    fn take_output(&mut self) -> (String, String) {
        let read = |reader: Option<JoinHandle<std::io::Result<Vec<u8>>>>| {
            reader
                .map(|reader| reader.join().expect("join router pipe drain"))
                .transpose()
                .unwrap_or_default()
                .unwrap_or_default()
        };
        (
            String::from_utf8_lossy(&read(self.stdout.take())).into_owned(),
            String::from_utf8_lossy(&read(self.stderr.take())).into_owned(),
        )
    }
}

impl Drop for SystemServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Drop for BusyDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn handle(mut stream: UnixStream, reply: &str, requests: &Mutex<Vec<WireRequest>>) {
    let timeout = Some(Duration::from_millis(300));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let mut header = [0_u8; 4];
    if stream.read_exact(&mut header).is_err() {
        return;
    }
    let length = u32::from_be_bytes(header) as usize;
    if length > REQUEST_LIMIT {
        return;
    }
    let mut frame = vec![0; length];
    if stream.read_exact(&mut frame).is_err() {
        return;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&frame) else {
        return;
    };
    if value["command"] != "Arguments" {
        return;
    }
    let Some(request_id) = value["arguments"]["context"]["request_id"].as_str() else {
        return;
    };
    let Some(args) = value["arguments"]["args"].as_array() else {
        return;
    };
    let args = args
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect();
    requests.lock().unwrap().push(WireRequest {
        request_id: request_id.to_owned(),
        args,
    });
    write_frame(&mut stream, reply.as_bytes());
}

fn write_frame(stream: &mut UnixStream, bytes: &[u8]) {
    if bytes.len() > 4 * 1024 * 1024 {
        return;
    }
    let _ = stream.write_all(&(bytes.len() as u32).to_be_bytes());
    let _ = stream.write_all(bytes);
    let _ = stream.flush();
}

fn request_stop(socket: &Path) {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return;
    };
    let timeout = Some(Duration::from_secs(1));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    write_frame(&mut stream, br#"{"command":"Stop"}"#);
    let mut header = [0_u8; 4];
    if stream.read_exact(&mut header).is_ok() {
        let length = u32::from_be_bytes(header) as usize;
        if length <= 4 * 1024 * 1024 {
            let mut response = vec![0; length];
            let _ = stream.read_exact(&mut response);
        }
    }
}

fn drain(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn has_option_path(args: &[String], option: &str, path: &Path) -> bool {
    let path = path.to_string_lossy();
    args.windows(2)
        .any(|pair| pair[0] == option && pair[1] == path)
}
