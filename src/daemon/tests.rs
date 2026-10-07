mod spool_transport_tests;

use super::reconciler::{
    DEBOUNCE, MAX_DEBOUNCE, RECONCILE_INTERVAL, ReconcileSchedule, WATCHED_RECONCILE_INTERVAL,
    indexed_path,
};
use super::*;
use std::io::Cursor;
use std::sync::{Arc, Barrier};
use std::time::{Instant, SystemTime};

pub(super) fn scratch() -> tempfile::TempDir {
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
    let mut schedule = ReconcileSchedule::new(start, RECONCILE_INTERVAL);
    assert!(!schedule.due(start, true));
    assert!(!schedule.due(start + DEBOUNCE / 2, false));
    assert!(schedule.due(start + DEBOUNCE, false));
    let mut schedule = ReconcileSchedule::new(start, RECONCILE_INTERVAL);
    for millisecond in (0..1000).step_by(20) {
        assert!(!schedule.due(start + Duration::from_millis(millisecond), true));
    }
    assert!(schedule.due(start + MAX_DEBOUNCE, true));
    let mut schedule = ReconcileSchedule::new(start, RECONCILE_INTERVAL);
    assert!(schedule.due(start + RECONCILE_INTERVAL, false));
    let mut watched = ReconcileSchedule::new(start, WATCHED_RECONCILE_INTERVAL);
    assert!(!watched.due(start + RECONCILE_INTERVAL, false));
    assert!(watched.due(start + WATCHED_RECONCILE_INTERVAL, false));
}

#[test]
fn watcher_ignores_churn_in_pruned_build_and_vcs_trees() {
    let root = Path::new("/repo");
    assert!(indexed_path(root, Path::new("/repo/src/lib.rs")));
    assert!(indexed_path(root, Path::new("/elsewhere/x")));
    for pruned in [
        "/repo/target/debug/x.o",
        "/repo/.git/index",
        "/repo/web/node_modules/a/b.js",
    ] {
        assert!(!indexed_path(root, Path::new(pruned)), "{pruned}");
    }
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

#[test]
fn encoded_request_limit_counts_json_escaping() {
    let context = crate::diagnostics::RequestContext::new(None, None);
    let args = vec!["board".to_owned(), "\"".repeat(32 * 1024)];
    let bytes = super::request_encoded_size(&args, &context).unwrap();
    assert!(args.iter().map(String::len).sum::<usize>() < super::MAX_DAEMON_REQUEST_BYTES);
    assert!(bytes > super::MAX_DAEMON_REQUEST_BYTES);
    let request = super::arguments(&args, &context);
    assert_eq!(bytes, serde_json::to_vec(&request).unwrap().len());
}
