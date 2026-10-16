use super::super::deadline::QueryDeadline;
use super::super::protocol::REQUEST_LIMIT;
use std::io::{self, ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const READ_SLICE: usize = 8 * 1024;

pub(super) struct PendingFrame {
    stream: UnixStream,
    accepted_at: Instant,
    frame_budget: Duration,
    deadline: QueryDeadline,
    header: [u8; 4],
    header_read: usize,
    bytes: Vec<u8>,
    payload_read: usize,
    expected_payload: Option<usize>,
    failure: Option<FrameFailure>,
}

pub(super) struct CompleteFrame {
    pub stream: UnixStream,
    pub bytes: Vec<u8>,
    pub deadline: QueryDeadline,
}

pub(super) enum FrameFailure {
    TimedOut(io::Error),
    InvalidLength(usize),
    Io(io::Error),
}

pub(super) enum FinishedFrame {
    Complete(CompleteFrame),
    Failed(UnixStream, FrameFailure),
}

impl PendingFrame {
    pub(super) fn new(
        stream: UnixStream,
        deadline: QueryDeadline,
        frame_budget: Duration,
    ) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            accepted_at: Instant::now(),
            frame_budget,
            deadline,
            header: [0; 4],
            header_read: 0,
            bytes: Vec::new(),
            payload_read: 0,
            expected_payload: None,
            failure: None,
        })
    }

    /// Reads at most one bounded slice. The caller polls every pending peer
    /// once per accept-loop turn, so a slow peer cannot hold the loop.
    pub(super) fn poll(&mut self) -> bool {
        if self.failure.is_some() || self.is_complete() {
            return true;
        }
        if self.accepted_at.elapsed() >= self.frame_budget || self.deadline.time_expired() {
            self.failure = Some(FrameFailure::TimedOut(reading_frame_timeout()));
            return true;
        }

        let result = if self.header_read < self.header.len() {
            self.stream.read(&mut self.header[self.header_read..])
        } else {
            let expected = self.expected_payload.expect("validated frame length");
            let remaining = expected.saturating_sub(self.payload_read);
            let count = remaining.min(READ_SLICE);
            self.stream
                .read(&mut self.bytes[4 + self.payload_read..4 + self.payload_read + count])
        };
        match result {
            Ok(0) => {
                self.failure = Some(FrameFailure::Io(io::Error::from(ErrorKind::UnexpectedEof)))
            }
            Ok(read) if self.header_read < self.header.len() => {
                self.header_read += read;
                if self.header_read == self.header.len() {
                    let length = u32::from_be_bytes(self.header) as usize;
                    if length > REQUEST_LIMIT {
                        self.failure = Some(FrameFailure::InvalidLength(length));
                    } else {
                        self.expected_payload = Some(length);
                        self.bytes.reserve_exact(4 + length);
                        self.bytes.extend_from_slice(&self.header);
                        self.bytes.resize(4 + length, 0);
                    }
                }
            }
            Ok(read) => self.payload_read += read,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == ErrorKind::TimedOut => {
                self.failure = Some(FrameFailure::TimedOut(reading_frame_timeout()))
            }
            Err(error) => self.failure = Some(FrameFailure::Io(error)),
        }
        if self.failure.is_some() || self.is_complete() {
            true
        } else if self.accepted_at.elapsed() >= self.frame_budget || self.deadline.time_expired() {
            self.failure = Some(FrameFailure::TimedOut(reading_frame_timeout()));
            true
        } else {
            false
        }
    }

    pub(super) fn finish(self) -> FinishedFrame {
        if let Some(failure) = self.failure {
            return FinishedFrame::Failed(self.stream, failure);
        }
        FinishedFrame::Complete(CompleteFrame {
            stream: self.stream,
            bytes: self.bytes,
            deadline: self.deadline,
        })
    }

    #[cfg(test)]
    pub(super) fn buffered_len(&self) -> usize {
        self.bytes.len()
    }

    fn is_complete(&self) -> bool {
        self.expected_payload
            .is_some_and(|expected| self.payload_read == expected)
    }
}

fn reading_frame_timeout() -> io::Error {
    io::Error::new(ErrorKind::TimedOut, "timed_out: phase=reading_frame")
}
