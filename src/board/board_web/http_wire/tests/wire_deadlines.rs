use super::*;
use std::{os::fd::AsRawFd, thread};

#[test]
fn acceptance_deadline_includes_queue_time_even_if_request_is_buffered() {
    let (mut server, mut client) = tcp_pair();
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
        .unwrap();
    let accepted = Instant::now() - REQUEST_TIMEOUT - Duration::from_millis(1);
    assert_eq!(read_request(&mut server, accepted).unwrap_err().status, 408);
}

#[test]
fn dripping_header_bytes_cannot_reset_whole_request_deadline() {
    let (mut server, mut client) = tcp_pair();
    let producer = thread::spawn(move || {
        for byte in b"GET / HTTP/1.1\r\nHost: local\r\n\r\n" {
            if client.write_all(&[*byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    let started = Instant::now();
    let error = read_request_until(&mut server, started + Duration::from_millis(120)).unwrap_err();
    assert_eq!(error.status, 408);
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(server);
    producer.join().unwrap();
}

#[test]
fn body_reads_share_the_original_header_deadline() {
    let (mut server, mut client) = tcp_pair();
    client
        .write_all(b"POST / HTTP/1.1\r\nHost: local\r\nContent-Length: 50\r\n\r\n")
        .unwrap();
    let producer = thread::spawn(move || {
        for _ in 0..50 {
            if client.write_all(b"x").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    let started = Instant::now();
    let error = read_request_until(&mut server, started + Duration::from_millis(120)).unwrap_err();
    assert_eq!(error.status, 408);
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(server);
    producer.join().unwrap();
}

#[test]
fn slow_response_peer_has_a_total_write_deadline() {
    let (mut server, _unread_client) = tcp_pair();
    let send_buffer: libc::c_int = 4096;
    // SAFETY: the live socket and pointer/length describe an initialized integer.
    let result = unsafe {
        libc::setsockopt(
            server.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&send_buffer as *const libc::c_int).cast(),
            std::mem::size_of_val(&send_buffer) as libc::socklen_t,
        )
    };
    assert_eq!(result, 0);
    let body = vec![b'x'; 2 * 1024 * 1024];
    let started = Instant::now();
    let error = response_write::send_response_until(
        &mut server,
        200,
        "application/json",
        &body,
        true,
        started + Duration::from_millis(100),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn even_buffered_crlf_requests_reject_hidden_lone_newlines() {
    rejected(
        b"GET / HTTP/1.1\r\nHost: local\r\nX-Test: safe\nInjected: bad\r\n\r\n",
        400,
    );
}
