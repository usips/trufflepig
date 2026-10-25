use super::reconciler::{
    DEBOUNCE, MAX_DEBOUNCE, RECONCILE_INTERVAL, ReconcileSchedule, WATCHED_RECONCILE_INTERVAL,
    indexed_path,
};
use super::*;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

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
fn daemon_shutdown_unlocks_inherited_file_description() {
    let scratch = scratch();
    let socket = DaemonSocket::bind(scratch.path()).unwrap();
    let inherited = socket._lock.try_clone().unwrap();
    drop(socket);
    let replacement = DaemonSocket::bind(scratch.path()).unwrap();
    drop(inherited);
    assert!(replacement.path.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn daemon_bind_preserves_a_socket_when_its_backlog_probe_times_out() {
    let scratch = scratch();
    let path = scratch.path().join(SOCKET_NAME);
    let listener = socket_connect::backlog_zero_listener(&path);
    let first =
        socket_connect::connect_by(&path, &QueryDeadline::after(Duration::from_secs(1))).unwrap();
    let original_inode = fs::symlink_metadata(&path).unwrap().ino();

    let error = DaemonSocket::bind(scratch.path())
        .err()
        .expect("a full listener backlog must not be treated as a stale socket");

    assert!(
        error
            .to_string()
            .contains("inspect existing repository socket")
    );
    let metadata = fs::symlink_metadata(&path).unwrap();
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.ino(), original_inode);
    drop(first);
    drop(listener);
}
