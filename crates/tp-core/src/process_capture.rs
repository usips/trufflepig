//! Bounded subprocess capture with process-group cleanup.

use crate::query_deadline::QueryDeadline;
use std::{
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    time::Duration,
};

const DRAIN_CHUNK_BYTES: usize = 8192;
const DRAIN_CHUNKS_PER_TICK: usize = 32;
const POLL_INTERVAL: Duration = Duration::from_millis(2);

pub struct CapturedProcess {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
}

pub enum CaptureError {
    Spawn(io::Error),
    MissingPipe(&'static str),
    ConfigurePipe(io::Error),
    Io(io::Error),
    TimedOut,
    OutputLimit,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "start subprocess: {error}"),
            Self::MissingPipe(stream) => write!(formatter, "subprocess {stream} pipe unavailable"),
            Self::ConfigurePipe(error) => write!(formatter, "configure subprocess pipe: {error}"),
            Self::Io(error) => write!(formatter, "subprocess pipe I/O: {error}"),
            Self::TimedOut => formatter.write_str("subprocess deadline expired"),
            Self::OutputLimit => formatter.write_str("subprocess output exceeds resource limit"),
        }
    }
}

impl std::fmt::Debug for CaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for CaptureError {}

/// Captures one command's output while the deadline and output cap remain live.
pub fn capture(
    command: &mut Command,
    input: Option<&[u8]>,
    deadline: &QueryDeadline,
    output_limit: usize,
) -> Result<CapturedProcess, CaptureError> {
    if deadline.expired() {
        return Err(CaptureError::TimedOut);
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = OwnedProcessGroup(command.spawn().map_err(CaptureError::Spawn)?);
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or(CaptureError::MissingPipe("stdout"))?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .ok_or(CaptureError::MissingPipe("stderr"))?;
    let mut stdin = child.0.stdin.take();
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    if let Some(pipe) = &stdin {
        nonblocking(pipe)?;
    }

    let mut remaining = input.unwrap_or_default();
    let mut stdout_bytes = Vec::with_capacity(DRAIN_CHUNK_BYTES.min(output_limit));
    let mut stderr_bytes = Vec::with_capacity(DRAIN_CHUNK_BYTES.min(output_limit));
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    loop {
        if deadline.expired() {
            return Err(CaptureError::TimedOut);
        }
        if let Some(pipe) = &mut stdin {
            if remaining.is_empty() {
                stdin = None;
            } else {
                match pipe.write(remaining) {
                    Ok(count) => remaining = &remaining[count..],
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => remaining = &[],
                    Err(error) => return Err(CaptureError::Io(error)),
                }
                if remaining.is_empty() {
                    stdin = None;
                }
            }
        }
        drain_pipe(
            &mut stdout,
            &mut stdout_bytes,
            &mut stdout_closed,
            output_limit.saturating_sub(stderr_bytes.len()),
        )?;
        drain_pipe(
            &mut stderr,
            &mut stderr_bytes,
            &mut stderr_closed,
            output_limit.saturating_sub(stdout_bytes.len()),
        )?;
        if let Some(status) = child.0.try_wait().map_err(CaptureError::Io)? {
            if stdout_closed && stderr_closed {
                return Ok(CapturedProcess {
                    stdout: stdout_bytes,
                    stderr: stderr_bytes,
                    status,
                });
            }
        }
        std::thread::sleep(deadline.cap(POLL_INTERVAL));
    }
}

struct OwnedProcessGroup(Child);

impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> Result<(), CaptureError> {
    let descriptor = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(CaptureError::ConfigurePipe(io::Error::last_os_error()));
    }
    Ok(())
}

fn drain_pipe(
    pipe: &mut impl Read,
    output: &mut Vec<u8>,
    closed: &mut bool,
    limit: usize,
) -> Result<(), CaptureError> {
    let mut buffer = [0; DRAIN_CHUNK_BYTES];
    for _ in 0..DRAIN_CHUNKS_PER_TICK {
        match pipe.read(&mut buffer) {
            Ok(0) => {
                *closed = true;
                break;
            }
            Ok(count) => {
                if output.len().saturating_add(count) > limit {
                    return Err(CaptureError::OutputLimit);
                }
                output.reserve(count);
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(CaptureError::Io(error)),
        }
    }
    Ok(())
}
