use super::backlog_listener::backlog_zero_listener;
use super::*;
use crate::daemon_transport::DeadlineWrite;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("trufflepig-connect-")
        .tempdir_in(std::env::var_os("TMPDIR").expect("TMPDIR must name disk-backed scratch"))
        .unwrap()
}

#[test]
fn connect_by_transfers_data_on_a_filesystem_socket() {
    let scratch = scratch();
    let path = scratch.path().join("daemon.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 4];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").unwrap();
    });
    let mut client = connect_by(&path, &QueryDeadline::after(Duration::from_secs(2))).unwrap();
    client.write_all(b"ping").unwrap();
    let mut reply = [0; 4];
    client.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"pong");
    server.join().unwrap();
}

#[test]
fn connect_by_preserves_absent_and_refused_socket_errors() {
    let scratch = scratch();
    let absent = scratch.path().join("absent.sock");
    assert_eq!(
        connect_by(&absent, &QueryDeadline::after(Duration::from_secs(1)))
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    assert!(
        !super::super::running_by(
            scratch.path(),
            &QueryDeadline::after(Duration::from_secs(1))
        )
        .unwrap()
    );

    let refused = scratch.path().join("refused.sock");
    drop(UnixListener::bind(&refused).unwrap());
    assert_eq!(
        connect_by(&refused, &QueryDeadline::after(Duration::from_secs(1)))
            .unwrap_err()
            .kind(),
        ErrorKind::ConnectionRefused
    );
}

#[test]
fn connect_by_rejects_an_expired_deadline_before_connecting() {
    let scratch = scratch();
    let path = scratch.path().join("daemon.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let error = super::super::running_by(scratch.path(), &QueryDeadline::after(Duration::ZERO))
        .unwrap_err();
    assert!(error.to_string().starts_with("timed_out: phase=connecting"));
    let error = connect_by(&path, &QueryDeadline::after(Duration::ZERO)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == ErrorKind::WouldBlock
    ));
}

#[test]
fn connect_by_rejects_abstract_and_oversized_paths() {
    let abstract_path = PathBuf::from(std::ffi::OsString::from_vec(b"\0daemon".to_vec()));
    assert_eq!(
        socket_address(&abstract_path).err().unwrap().kind(),
        ErrorKind::InvalidInput
    );
    let capacity =
        std::mem::size_of::<libc::sockaddr_un>() - std::mem::size_of::<libc::sa_family_t>();
    let oversized = PathBuf::from("x".repeat(capacity));
    assert_eq!(
        socket_address(&oversized).err().unwrap().kind(),
        ErrorKind::InvalidInput
    );
}

#[cfg(target_os = "linux")]
#[test]
fn connect_by_times_out_instead_of_accepting_a_full_backlog_as_connected() {
    let scratch = scratch();
    let path = scratch.path().join("daemon.sock");
    let listener = backlog_zero_listener(&path);
    let first = connect_by(&path, &QueryDeadline::after(Duration::from_secs(1))).unwrap();
    let started = Instant::now();
    let error = super::super::running_by(
        scratch.path(),
        &QueryDeadline::after(Duration::from_millis(250)),
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("timed_out: phase=connecting"));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(super::super::running(scratch.path()));
    drop(first);
    drop(listener);
}

#[cfg(target_os = "linux")]
#[test]
fn connect_by_cancels_while_the_listener_backlog_is_full() {
    let scratch = scratch();
    let path = scratch.path().join("daemon.sock");
    let listener = backlog_zero_listener(&path);
    let first = connect_by(&path, &QueryDeadline::after(Duration::from_secs(1))).unwrap();
    let (cancel_peer, cancel_handle) = UnixStream::pair().unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::new(cancel_peer));
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        drop(cancel_handle);
    });
    let cancelled_at = Instant::now();
    let error =
        connect_by(&path, &deadline).expect_err("cancelled full-backlog connect must not succeed");
    cancel.join().unwrap();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(cancelled_at.elapsed() < Duration::from_secs(2));
    drop(first);
    drop(listener);
}

#[test]
fn deadline_write_cancels_after_partial_socket_pair_progress() {
    let (mut writer_stream, mut peer_stream) = UnixStream::pair().unwrap();
    let send_buffer = 4096_i32;
    // SAFETY: the option pointer references an initialized integer of the given length.
    let configured = unsafe {
        libc::setsockopt(
            writer_stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&send_buffer as *const i32).cast(),
            std::mem::size_of_val(&send_buffer) as libc::socklen_t,
        )
    };
    assert_eq!(configured, 0, "{}", io::Error::last_os_error());
    peer_stream.set_nonblocking(true).unwrap();
    let (cancel_peer, cancel_handle) = UnixStream::pair().unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::new(cancel_peer));
    let cancel_observer = peer_stream.try_clone().unwrap();
    let cancel = std::thread::spawn(move || {
        let mut readiness = libc::pollfd {
            fd: cancel_observer.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: readiness points to one initialized pollfd and remains live for poll.
        let observed = unsafe { libc::poll(&mut readiness, 1, 1_000) } > 0
            && readiness.revents & libc::POLLIN != 0;
        drop(cancel_handle);
        observed
    });
    let bytes = vec![0x5a; 1024 * 1024];
    let started = Instant::now();
    let error = DeadlineWrite::new(&mut writer_stream, &deadline)
        .write_all(&bytes)
        .unwrap_err();
    assert!(
        cancel.join().unwrap(),
        "cancel only after the peer can observe partial request bytes"
    );
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(2));
    let mut received = vec![0; bytes.len()];
    let received = peer_stream.read(&mut received).unwrap();
    assert!(
        received > 0,
        "some frame bytes should be written before cancellation"
    );
    assert!(
        received < bytes.len(),
        "the blocked write must not complete"
    );
}
