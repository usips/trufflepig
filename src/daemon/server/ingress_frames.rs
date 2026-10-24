use super::super::deadline::QueryDeadline;
use super::super::protocol::REQUEST_LIMIT;
use std::io::{self, ErrorKind, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

/// Bytes one pending frame may read in one accept-loop turn.
const READ_SLICE: usize = 8 * 1024;
/// How long a refused connection's request is read before its socket closes.
const REFUSAL_DRAIN: Duration = Duration::from_millis(200);

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

/// What one bounded read turn did to a pending frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FramePoll {
    /// The peer sent nothing this turn.
    Idle,
    /// Bytes arrived, but the frame is still incomplete.
    Progressed,
    /// The frame completed or failed; `finish` consumes it.
    Finished,
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

    /// Reads until the peer would block, the frame completes or fails, or this
    /// turn's `READ_SLICE` is spent. A frame already buffered therefore finishes
    /// in one call, and a peer's slice bounds its share of each accept-loop turn.
    pub(super) fn poll(&mut self) -> FramePoll {
        let mut slice = READ_SLICE;
        let mut progressed = false;
        while !self.is_finished() && slice > 0 && !self.expired() {
            match self.read_some(slice) {
                Ok(0) => {
                    self.failure = Some(FrameFailure::Io(io::Error::from(ErrorKind::UnexpectedEof)))
                }
                Ok(read) => {
                    progressed = true;
                    slice -= read;
                    self.record(read);
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::TimedOut => {
                    self.failure = Some(FrameFailure::TimedOut(reading_frame_timeout()))
                }
                Err(error) => self.failure = Some(FrameFailure::Io(error)),
            }
        }
        if !self.is_finished() && self.expired() {
            self.failure = Some(FrameFailure::TimedOut(reading_frame_timeout()));
        }
        if self.is_finished() {
            FramePoll::Finished
        } else if progressed {
            FramePoll::Progressed
        } else {
            FramePoll::Idle
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

    /// The peer's descriptor, for readiness waits in the accept loop.
    pub(super) fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    #[cfg(test)]
    pub(super) fn buffered_len(&self) -> usize {
        self.bytes.len()
    }

    fn is_finished(&self) -> bool {
        self.failure.is_some() || self.is_complete()
    }

    fn is_complete(&self) -> bool {
        self.expected_payload
            .is_some_and(|expected| self.payload_read == expected)
    }

    fn expired(&self) -> bool {
        self.accepted_at.elapsed() >= self.frame_budget || self.deadline.time_expired()
    }

    /// Reads into the header until it is complete, then into the payload.
    fn read_some(&mut self, slice: usize) -> io::Result<usize> {
        if self.header_read < self.header.len() {
            self.stream.read(&mut self.header[self.header_read..])
        } else {
            let expected = self.expected_payload.expect("validated frame length");
            let start = 4 + self.payload_read;
            let count = expected.saturating_sub(self.payload_read).min(slice);
            self.stream.read(&mut self.bytes[start..start + count])
        }
    }

    fn record(&mut self, read: usize) {
        if self.header_read < self.header.len() {
            self.header_read += read;
            if self.header_read == self.header.len() {
                self.start_payload();
            }
        } else {
            self.payload_read += read;
        }
    }

    /// Validates the length from a complete header before any payload allocation.
    fn start_payload(&mut self) {
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

/// Blocks until the listener or a pending or refused peer is readable, at most
/// `timeout`, so an idle accept loop wakes as soon as work arrives.
pub(super) fn wait_for_ingress(
    listener: &UnixListener,
    pending: &[PendingFrame],
    refused: &[PendingFrame],
    timeout: Duration,
) {
    let mut readiness = Vec::with_capacity(pending.len() + refused.len() + 1);
    readiness.push(readable(listener.as_raw_fd()));
    readiness.extend(
        pending
            .iter()
            .chain(refused)
            .map(|frame| readable(frame.raw_fd())),
    );
    let timeout_ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `readiness` holds initialized pollfd entries whose descriptors are
    // owned by `listener` or by frames borrowed for the duration of this call.
    unsafe {
        libc::poll(
            readiness.as_mut_ptr(),
            readiness.len() as libc::nfds_t,
            timeout_ms,
        )
    };
}

/// Reads refused requests, so closing a socket with unread bytes does not reset
/// the refusal reply. A request that completes or outlives `REFUSAL_DRAIN` closes here.
pub(super) fn poll_refused(refused: &mut Vec<PendingFrame>) -> bool {
    let mut progressed = false;
    refused.retain_mut(|frame| match frame.poll() {
        FramePoll::Idle => true,
        FramePoll::Progressed => {
            progressed = true;
            true
        }
        FramePoll::Finished => {
            progressed = true;
            false
        }
    });
    progressed
}

/// Holds a refused connection open for its request, bounded by `capacity`.
pub(super) fn hold_refused(refused: &mut Vec<PendingFrame>, stream: UnixStream, capacity: usize) {
    if refused.len() == capacity {
        return;
    }
    let deadline = QueryDeadline::after(REFUSAL_DRAIN);
    if let Ok(frame) = PendingFrame::new(stream, deadline, REFUSAL_DRAIN) {
        refused.push(frame);
    }
}

fn readable(fd: RawFd) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }
}

fn reading_frame_timeout() -> io::Error {
    io::Error::new(ErrorKind::TimedOut, coded!(TimedOut, "phase=reading_frame"))
}
