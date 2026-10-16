use super::deadline::QueryDeadline;
use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const TRANSPORT_IO_SLICE: Duration = Duration::from_millis(100);

/// A reply-read timeout; `deadline_ms` is the relative configured reply wait budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DaemonReplyTimeout {
    wait_ms: u64,
    deadline_ms: u64,
    retry_after_ms: u64,
}

impl DaemonReplyTimeout {
    pub(crate) fn new(wait: Duration, deadline: Duration) -> Self {
        Self {
            wait_ms: milliseconds(wait),
            deadline_ms: milliseconds(deadline),
            retry_after_ms: 250,
        }
    }

    pub(crate) fn from_error(error: &anyhow::Error) -> Option<&Self> {
        error.chain().find_map(|cause| cause.downcast_ref::<Self>())
    }

    pub(crate) fn from_message(message: &str) -> Option<Self> {
        const PREFIX: &str = "daemon_reply_timeout: phase=awaiting_reply wait_ms=";
        let fields = message.strip_prefix(PREFIX)?;
        let fields = fields.strip_suffix(" replay=read_only")?;
        let (wait_ms, fields) = fields.split_once(" deadline_ms=")?;
        let (deadline_ms, retry_after_ms) = fields.split_once(" retry_after_ms=")?;
        Some(Self {
            wait_ms: canonical_milliseconds(wait_ms)?,
            deadline_ms: canonical_milliseconds(deadline_ms)?,
            retry_after_ms: canonical_milliseconds(retry_after_ms)?,
        })
    }

    pub(crate) fn retry_after(&self) -> Duration {
        Duration::from_millis(self.retry_after_ms)
    }

    #[cfg(test)]
    pub(crate) fn elapsed_wait(&self) -> Duration {
        Duration::from_millis(self.wait_ms)
    }

    #[cfg(test)]
    pub(crate) fn wait_budget(&self) -> Duration {
        Duration::from_millis(self.deadline_ms)
    }
}

impl fmt::Display for DaemonReplyTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon_reply_timeout: phase=awaiting_reply wait_ms={} deadline_ms={} retry_after_ms={} replay=read_only",
            self.wait_ms, self.deadline_ms, self.retry_after_ms
        )
    }
}

impl std::error::Error for DaemonReplyTimeout {}

fn canonical_milliseconds(value: &str) -> Option<u64> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse::<u64>().ok()
}

fn milliseconds(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

pub(crate) struct DeadlineRead<'a> {
    stream: &'a mut UnixStream,
    deadline: &'a QueryDeadline,
}

impl<'a> DeadlineRead<'a> {
    pub(crate) fn new(stream: &'a mut UnixStream, deadline: &'a QueryDeadline) -> Self {
        Self { stream, deadline }
    }
}

impl Read for DeadlineRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            let wait = self.deadline.cap(TRANSPORT_IO_SLICE);
            if wait.is_zero() {
                return Err(read_deadline_expired());
            }
            self.stream.set_read_timeout(Some(wait))?;
            let result = self.stream.read(buffer);
            if self.deadline.expired() {
                return Err(read_deadline_expired());
            }
            match result {
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                result => return result,
            }
        }
    }
}

fn read_deadline_expired() -> io::Error {
    io::Error::new(ErrorKind::TimedOut, "daemon reply deadline expired")
}

pub(crate) struct DeadlineWrite<'a> {
    stream: &'a mut UnixStream,
    deadline: &'a QueryDeadline,
}

impl<'a> DeadlineWrite<'a> {
    pub(crate) fn new(stream: &'a mut UnixStream, deadline: &'a QueryDeadline) -> Self {
        Self { stream, deadline }
    }
}

impl Write for DeadlineWrite<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            let wait = self.deadline.cap(TRANSPORT_IO_SLICE);
            if wait.is_zero() {
                return Err(write_deadline_expired());
            }
            self.stream.set_write_timeout(Some(wait))?;
            let result = self.stream.write(buffer);
            if self.deadline.expired() {
                return Err(write_deadline_expired());
            }
            match result {
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.deadline.expired() {
            return Err(write_deadline_expired());
        }
        let result = self.stream.flush();
        if self.deadline.expired() {
            return Err(write_deadline_expired());
        }
        result
    }
}

fn write_deadline_expired() -> io::Error {
    io::Error::new(ErrorKind::TimedOut, "daemon request write deadline expired")
}
