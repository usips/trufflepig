use super::{WorkerStatus, lease, protocol};
use crate::semantic::runtime_config::InferenceConfig;
use anyhow::{Context, Result, bail};
use std::{
    io::{self, ErrorKind},
    os::unix::net::UnixStream,
    os::{
        fd::{FromRawFd, RawFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const IO_SLICE: Duration = Duration::from_millis(25);

/// End-to-end deadline for one rerank request through the shared worker.
pub const RERANK_DEADLINE: Duration = Duration::from_millis(1500);

/// Score (query, document) pairs through the shared worker, bounded to 1.5 s.
/// Never loads a model in this process.
pub fn rerank_query(cache_identity: &Path, query: &str, documents: &[String]) -> Result<Vec<f32>> {
    #[cfg(not(feature = "semantic"))]
    {
        let _ = (cache_identity, query, documents);
        return Err(crate::semantic::unavailable());
    }
    let started = Instant::now();
    let deadline_instant = started + RERANK_DEADLINE;
    protocol::validate_rerank_inputs(query, documents)?;
    let config = InferenceConfig::load()?;
    let cache = super::lease::worker_cache_dir()?;
    let remaining = deadline_instant.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("semantic_timeout: request exceeded its deadline before send");
    }
    let deadline_ms = SystemTime::now()
        .checked_add(remaining)
        .context("semantic_worker: system clock overflow")?
        .duration_since(UNIX_EPOCH)?
        .as_millis() as u64;
    let request = protocol::WorkerRequest::new(
        &config,
        protocol::WorkerCommand::Rerank {
            root_id: cache_identity.to_string_lossy().into_owned(),
            query: query.to_owned(),
            documents: documents.to_vec(),
            deadline_ms,
        },
    );
    let mut stream = match connect(&cache, deadline_instant) {
        Ok(stream) => stream,
        Err(error)
            if error.kind() == ErrorKind::NotFound
                || error.kind() == ErrorKind::ConnectionRefused =>
        {
            let _ = super::ensure_started_until(cache_identity, deadline_instant)?;
            bail!("semantic_loading: inference worker is starting")
        }
        Err(error) => return Err(error.into()),
    };
    configure_io(&stream, deadline_instant)?;
    protocol::write_request_with_deadline(&mut stream, &request, deadline_instant)?;
    match protocol::read_reply_with_deadline(&mut stream, deadline_instant)? {
        protocol::WorkerReply::Scores { values } => Ok(values),
        protocol::WorkerReply::Error { message } => bail!("rerank_worker: {message}"),
        protocol::WorkerReply::Status(_) | protocol::WorkerReply::Embeddings { .. } => {
            bail!("rerank_worker: invalid rerank response")
        }
    }
}

pub(super) fn connect(cache: &Path, deadline: Instant) -> io::Result<UnixStream> {
    let path = cache.join(lease::SOCKET_NAME);
    let bytes = path.as_os_str().as_bytes();
    let address = unix_address(bytes)?;
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = SocketFd(fd);
    let address_length = address_length(bytes.len());
    let result = unsafe {
        libc::connect(
            fd.0,
            (&address as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
            address_length,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        let in_progress = matches!(
            error.raw_os_error(),
            Some(code) if code == libc::EINPROGRESS || code == libc::EALREADY
        );
        if !in_progress {
            return Err(error);
        }
        wait_for_connect(fd.0, deadline)?;
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd.0) };
    std::mem::forget(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

pub(super) fn configure_io(stream: &UnixStream, deadline: Instant) -> io::Result<()> {
    let remaining = remaining(deadline)?;
    let timeout = remaining.min(IO_SLICE);
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))
}

pub(super) fn request_status(
    cache: &Path,
    config: &InferenceConfig,
    deadline: Instant,
) -> Result<WorkerStatus> {
    let mut stream = connect(cache, deadline).map_err(anyhow::Error::from)?;
    configure_io(&stream, deadline)?;
    let request = protocol::WorkerRequest::new(config, protocol::WorkerCommand::Status);
    protocol::write_request_with_deadline(&mut stream, &request, deadline)?;
    match protocol::read_reply_with_deadline(&mut stream, deadline)? {
        protocol::WorkerReply::Status(status) => Ok(status),
        protocol::WorkerReply::Error { message } => bail!("{message}"),
        protocol::WorkerReply::Embeddings { .. } | protocol::WorkerReply::Scores { .. } => {
            bail!("semantic_worker: invalid status response")
        }
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::Error::new(
            ErrorKind::TimedOut,
            "worker IPC deadline exceeded",
        ))
    } else {
        Ok(remaining)
    }
}

fn unix_address(bytes: &[u8]) -> io::Result<libc::sockaddr_un> {
    let path_capacity =
        std::mem::size_of::<libc::sockaddr_un>() - std::mem::size_of::<libc::sa_family_t>();
    if bytes.is_empty() || bytes.len() >= path_capacity {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "worker socket path is too long",
        ));
    }
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            address.sun_path.as_mut_ptr().cast::<u8>(),
            bytes.len(),
        );
    }
    Ok(address)
}

fn address_length(path_length: usize) -> libc::socklen_t {
    (std::mem::size_of::<libc::sa_family_t>() + path_length + 1) as libc::socklen_t
}

fn wait_for_connect(fd: RawFd, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = remaining(deadline)?;
        let millis = remaining.as_millis().min(i32::MAX as u128).max(1) as i32;
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll_fd, 1, millis) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            continue;
        }
        if poll_fd.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        if poll_fd.revents & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) == 0 {
            continue;
        }
        let mut socket_error = 0_i32;
        let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut socket_error as *mut i32).cast(),
                &mut length,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if socket_error != 0 {
            return Err(io::Error::from_raw_os_error(socket_error));
        }
        return Ok(());
    }
}

struct SocketFd(RawFd);

impl Drop for SocketFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::io::AsRawFd, os::unix::net::UnixListener, thread};

    #[test]
    fn connect_to_full_backlog_returns_by_deadline() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(lease::SOCKET_NAME);
        let listener = UnixListener::bind(&path)?;
        let result = unsafe { libc::listen(listener.as_raw_fd(), 1) };
        assert_eq!(result, 0);
        let mut clients = Vec::new();
        for _ in 0..8 {
            match connect(directory.path(), Instant::now() + Duration::from_millis(20)) {
                Ok(stream) => clients.push(stream),
                Err(_) => break,
            }
        }
        assert!(!clients.is_empty());

        let started = Instant::now();
        let result = connect(directory.path(), started + Duration::from_millis(40));
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(clients);
        Ok(())
    }

    #[test]
    fn nonresponding_peer_is_bounded_by_absolute_deadline() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(lease::SOCKET_NAME);
        let listener = UnixListener::bind(&path)?;
        let acceptor = thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("accept client");
            thread::sleep(Duration::from_millis(100));
        });
        let deadline = Instant::now() + Duration::from_millis(35);
        let mut stream = connect(directory.path(), deadline)?;
        configure_io(&stream, deadline)?;
        let error = protocol::read_reply_with_deadline(&mut stream, deadline)
            .expect_err("silent peer must hit deadline");
        let message = format!("{error:#}");
        assert!(message.contains("deadline") || message.contains("timed out"));
        acceptor.join().expect("acceptor thread");
        Ok(())
    }
}
