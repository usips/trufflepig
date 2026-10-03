//! Close-delimited SSE writes have one absolute deadline per frame.

use std::{
    io::{self, Write},
    net::TcpStream,
    time::{Duration, Instant},
};

pub(super) const RESPONSE_HEADERS: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n";

pub(super) trait DeadlineWriter: Write {
    fn remaining_timeout(&self, timeout: Duration) -> io::Result<()>;
}

impl DeadlineWriter for TcpStream {
    fn remaining_timeout(&self, timeout: Duration) -> io::Result<()> {
        self.set_write_timeout(Some(timeout))
    }
}

pub(super) fn send(
    writer: &mut impl DeadlineWriter,
    mut bytes: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    let started = Instant::now();
    while !bytes.is_empty() {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "event send deadline exceeded",
            ));
        }
        writer.remaining_timeout(remaining)?;
        match writer.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "event socket closed",
                ));
            }
            Ok(sent) => bytes = &bytes[sent..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    if started.elapsed() > timeout {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "event send deadline exceeded",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn peer_connected(socket: &TcpStream) -> io::Result<bool> {
    let mut byte = [0];
    match socket.peek(&mut byte) {
        Ok(_) => Ok(false), // EOF or unexpected pipelined request bytes.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(true),
        Err(error) => Err(error),
    }
}
