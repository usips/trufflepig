use super::*;
use std::io::{Cursor, Write};
use std::sync::{Barrier, mpsc};

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("trufflepig-daemon-")
        .tempdir_in(std::env::var_os("TMPDIR").expect("TMPDIR must name disk-backed scratch space"))
        .unwrap()
}

#[test]
fn daemon_startup_lock_preserves_active_socket() {
    let scratch = scratch();
    let first = DaemonSocket::bind(scratch.path()).unwrap();
    let inode = fs::metadata(&first.path).unwrap().ino();
    assert!(DaemonSocket::bind(scratch.path()).is_err());
    assert_eq!(fs::metadata(&first.path).unwrap().ino(), inode);
    drop(first);
    assert!(!scratch.path().join(SOCKET_NAME).exists());
    assert!(DaemonSocket::bind(scratch.path()).is_ok());
}

#[test]
fn daemon_concurrent_startup_has_one_owner() {
    let scratch = scratch();
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = scratch.path().to_owned();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let socket = DaemonSocket::bind(&path);
                barrier.wait();
                socket.is_ok()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| usize::from(thread.join().unwrap()))
            .sum::<usize>(),
        1
    );
}

#[test]
fn daemon_does_not_remove_non_socket() {
    let scratch = scratch();
    let path = scratch.path().join(SOCKET_NAME);
    fs::write(&path, b"user data").unwrap();
    assert!(DaemonSocket::bind(scratch.path()).is_err());
    assert_eq!(fs::read(path).unwrap(), b"user data");
}

#[test]
fn daemon_preserves_unlocked_active_socket_and_recovers_stale_socket() {
    let scratch = scratch();
    let path = scratch.path().join(SOCKET_NAME);
    let listener = UnixListener::bind(&path).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    assert!(DaemonSocket::bind(scratch.path()).is_err());
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    drop(listener);
    assert!(DaemonSocket::bind(scratch.path()).is_ok());
}

#[test]
fn daemon_protocol_rejects_oversized_and_truncated_frames() {
    let header = ((protocol::REQUEST_LIMIT + 1) as u32).to_be_bytes();
    assert!(protocol::read_request(&mut Cursor::new(header)).is_err());
    let bytes = [0, 0, 0, 8, b'{'];
    assert!(protocol::read_request(&mut Cursor::new(bytes)).is_err());
    let request = DaemonRequest::Arguments {
        context: crate::diagnostics::RequestContext::new(None, None),
        args: vec![String::new(); 257],
    };
    assert!(protocol::write_request(&mut Vec::new(), &request).is_err());
}

#[test]
fn daemon_protocol_bounds_complete_serialized_response() {
    let reply = DaemonReply::Success {
        output: "\0".repeat(1024 * 1024),
    };
    let mut wire = Vec::new();
    protocol::write_reply(&mut wire, &reply).unwrap();
    assert!(wire.len() < 1024);
    assert!(matches!(
        protocol::read_reply(&mut Cursor::new(wire)).unwrap(),
        DaemonReply::Failure { .. }
    ));
}

#[test]
fn daemon_reconciliation_has_debounce_deadline_and_periodic_recovery() {
    let start = Instant::now();
    let mut schedule = ReconcileSchedule::new(start);
    assert!(!schedule.due(start, true));
    assert!(!schedule.due(start + DEBOUNCE / 2, false));
    assert!(schedule.due(start + DEBOUNCE, false));
    let mut schedule = ReconcileSchedule::new(start);
    for millisecond in (0..1000).step_by(20) {
        assert!(!schedule.due(start + Duration::from_millis(millisecond), true));
    }
    assert!(schedule.due(start + MAX_DEBOUNCE, true));
    let mut schedule = ReconcileSchedule::new(start);
    assert!(schedule.due(start + RECONCILE_INTERVAL, false));
}

#[test]
fn daemon_serves_recovers_protocol_errors_watches_and_stops() {
    let scratch = scratch();
    let root = scratch.path().join("repo");
    let cache = scratch.path().join("cache");
    fs::create_dir(&root).unwrap();
    let (sender, receiver) = mpsc::channel();
    let serve_root = root.clone();
    let serve_cache = cache.clone();
    let daemon = std::thread::spawn(move || {
        serve(&serve_root, &serve_cache, |args| match args {
            DaemonEvent::Request { args, .. } => Ok(args.join("|")),
            DaemonEvent::Idle => Ok(String::new()),
            DaemonEvent::Reconcile => {
                let _ = sender.send(());
                Ok(String::new())
            }
        })
    });
    receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut bad_client = UnixStream::connect(cache.join(SOCKET_NAME)).unwrap();
    bad_client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    bad_client
        .write_all(&((protocol::REQUEST_LIMIT + 1) as u32).to_be_bytes())
        .unwrap();
    let bad_reply = protocol::read_reply(&mut bad_client);
    let reply = request(
        &cache,
        &["search".to_owned(), "a\nb".to_owned()],
        &crate::diagnostics::RequestContext::new(None, None),
    );
    fs::write(root.join("changed.rs"), "fn changed() {}\n").unwrap();
    let watched = receiver.recv_timeout(Duration::from_secs(5));
    let stopped = stop(&cache);
    daemon.join().unwrap().unwrap();
    assert!(matches!(bad_reply.unwrap(), DaemonReply::Failure { .. }));
    assert_eq!(reply.unwrap().as_deref(), Some("search|a\nb"));
    assert!(watched.is_ok(), "watcher did not trigger reconciliation");
    assert!(stopped.unwrap().contains("stopped"));
    assert_eq!(
        request(
            &cache,
            &[],
            &crate::diagnostics::RequestContext::new(None, None)
        )
        .unwrap(),
        None
    );
}

#[test]
fn daemon_connect_unreachability_includes_sandbox_permission_denial() {
    assert!(unreachable(ErrorKind::NotFound));
    assert!(unreachable(ErrorKind::ConnectionRefused));
    assert!(unreachable(ErrorKind::PermissionDenied));
    assert!(!unreachable(ErrorKind::ConnectionReset));
    assert!(!unreachable(ErrorKind::TimedOut));
}

#[test]
fn daemon_shutdown_unlocks_inherited_file_description() {
    let scratch = scratch();
    let socket = DaemonSocket::bind(scratch.path()).unwrap();
    let inherited = socket._lock.try_clone().unwrap();
    drop(socket);
    let replacement = DaemonSocket::bind(scratch.path()).unwrap();
    drop(inherited);
    assert!(replacement.path.exists());
}
