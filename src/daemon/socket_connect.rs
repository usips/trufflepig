use crate::query_deadline::QueryDeadline;
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const CONNECT_WAIT_SLICE: Duration = Duration::from_millis(100);
const BACKLOG_RETRY_DELAY: Duration = Duration::from_millis(25);

pub(super) fn connect_by(path: &Path, deadline: &QueryDeadline) -> io::Result<UnixStream> {
    let (address, address_length) = socket_address(path)?;
    loop {
        check_deadline(deadline)?;
        let descriptor = socket()?;
        check_deadline(deadline)?;
        let result = connect_socket(&descriptor, &address, address_length);
        check_deadline(deadline)?;

        match result {
            Ok(()) => return connected_stream(descriptor, deadline),
            Err(error) if is_backlog_full(&error) => {
                drop(descriptor);
                wait_before_retry(deadline)?;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {
                drop(descriptor);
                wait_before_retry(deadline)?;
            }
            Err(error) if is_connect_pending(&error) => {
                match wait_for_connect(&descriptor, deadline) {
                    Ok(()) => return connected_stream(descriptor, deadline),
                    Err(error) if is_backlog_full(&error) => {
                        drop(descriptor);
                        wait_before_retry(deadline)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) if is_connected(&error) => return connected_stream(descriptor, deadline),
            Err(error) => return Err(error),
        }
    }
}

fn socket_address(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un contains only integer fields and a byte array; zero is valid.
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    let capacity = address.sun_path.len();
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= capacity {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "daemon socket path is not a valid filesystem path",
        ));
    }

    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    let length = libc::socklen_t::try_from(length)
        .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "daemon socket path is too long"))?;
    Ok((address, length))
}

fn socket() -> io::Result<OwnedFd> {
    // SAFETY: socket has no pointer arguments and returns a new descriptor on success.
    let descriptor = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this call created a fresh descriptor which is now owned exactly once.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn connect_socket(
    descriptor: &OwnedFd,
    address: &libc::sockaddr_un,
    address_length: libc::socklen_t,
) -> io::Result<()> {
    // SAFETY: descriptor is live and address remains valid for the complete syscall.
    let result = unsafe {
        libc::connect(
            descriptor.as_raw_fd(),
            (address as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
            address_length,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn wait_for_connect(descriptor: &OwnedFd, deadline: &QueryDeadline) -> io::Result<()> {
    loop {
        let wait = deadline.cap(CONNECT_WAIT_SLICE);
        if wait.is_zero() {
            return Err(connect_deadline_expired());
        }
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: wait.subsec_nanos().into(),
        };
        let mut readiness = libc::pollfd {
            fd: descriptor.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: readiness points to one initialized pollfd and timeout is live.
        let result = unsafe { libc::ppoll(&mut readiness, 1, &timeout, std::ptr::null()) };
        if deadline.expired() {
            return Err(connect_deadline_expired());
        }
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
        if readiness.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        if readiness.revents & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) == 0 {
            continue;
        }
        let socket_error = socket_error(descriptor)?;
        if socket_error != 0 {
            return Err(io::Error::from_raw_os_error(socket_error));
        }
        return Ok(());
    }
}

fn socket_error(descriptor: &OwnedFd) -> io::Result<i32> {
    let mut error = 0_i32;
    let mut length = std::mem::size_of_val(&error) as libc::socklen_t;
    // SAFETY: the output pointers reference initialized writable storage of the declared size.
    let result = unsafe {
        libc::getsockopt(
            descriptor.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&mut error as *mut i32).cast(),
            &mut length,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(error)
    }
}

fn connected_stream(descriptor: OwnedFd, deadline: &QueryDeadline) -> io::Result<UnixStream> {
    let stream = UnixStream::from(descriptor);
    stream.peer_addr()?;
    check_deadline(deadline)?;
    stream.set_nonblocking(false)?;
    check_deadline(deadline)?;
    Ok(stream)
}

fn wait_before_retry(deadline: &QueryDeadline) -> io::Result<()> {
    let wait = deadline.cap(BACKLOG_RETRY_DELAY);
    if wait.is_zero() {
        return Err(connect_deadline_expired());
    }
    std::thread::sleep(wait);
    check_deadline(deadline)
}

fn check_deadline(deadline: &QueryDeadline) -> io::Result<()> {
    if deadline.expired() {
        Err(connect_deadline_expired())
    } else {
        Ok(())
    }
}

fn connect_deadline_expired() -> io::Error {
    io::Error::new(ErrorKind::TimedOut, "daemon connect deadline expired")
}

fn is_backlog_full(error: &io::Error) -> bool {
    error.kind() == ErrorKind::WouldBlock || error.raw_os_error() == Some(libc::EAGAIN)
}

fn is_connect_pending(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code) if code == libc::EINPROGRESS || code == libc::EALREADY
    )
}

fn is_connected(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EISCONN)
}

#[cfg(test)]
mod tests;
