use super::*;

#[cfg(unix)]
#[test]
fn slow_tcp_subscriber_hits_absolute_send_deadline() {
    use std::os::fd::AsRawFd;
    let (mut server, _client) = socket_pair();
    let size: libc::c_int = 4096;
    // The socket belongs to this test; lowering its send buffer forces backpressure.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                server.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        },
        0
    );
    let bytes = vec![b'x'; 1024 * 1024];
    let started = Instant::now();
    let error = stream_socket::send(&mut server, &bytes, Duration::from_millis(80)).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn partial_writes_share_one_deadline_and_drop_their_permit_on_error() {
    struct PartialWriter {
        timeouts: Mutex<Vec<Duration>>,
    }
    impl Write for PartialWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            thread::sleep(Duration::from_millis(5));
            Ok(1)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl stream_socket::DeadlineWriter for PartialWriter {
        fn remaining_timeout(&self, timeout: Duration) -> io::Result<()> {
            self.timeouts.lock().unwrap().push(timeout);
            Ok(())
        }
    }
    let streams = streams(empty_reader());
    let permit = streams.reserve().unwrap();
    let mut writer = PartialWriter {
        timeouts: Mutex::new(Vec::new()),
    };
    assert_eq!(
        stream_socket::send(&mut writer, &[1; 100], Duration::from_millis(25))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    let timeouts = writer.timeouts.lock().unwrap();
    assert!(timeouts.windows(2).all(|pair| pair[0] > pair[1]));
    drop(permit);
    assert_eq!(streams.active(), 0);
}
