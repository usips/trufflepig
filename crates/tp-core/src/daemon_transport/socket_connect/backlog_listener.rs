//! A zero-backlog listener for tests that need a connect to stall.

use super::socket_address;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::path::Path;

/// A listening socket with a zero backlog, so the second connect cannot complete.
pub fn backlog_zero_listener(path: &Path) -> UnixListener {
    let (address, length) = socket_address(path).unwrap();
    // SAFETY: socket has no pointer arguments and returns a fresh descriptor on success.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    assert!(raw >= 0, "{}", io::Error::last_os_error());
    // SAFETY: raw is a fresh descriptor and becomes owned exactly once here.
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: descriptor is live and address is valid for the complete syscall.
    let bound = unsafe {
        libc::bind(
            descriptor.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
            length,
        )
    };
    assert_eq!(bound, 0, "{}", io::Error::last_os_error());
    // SAFETY: descriptor is a bound AF_UNIX stream socket.
    let listening = unsafe { libc::listen(descriptor.as_raw_fd(), 0) };
    assert_eq!(listening, 0, "{}", io::Error::last_os_error());
    UnixListener::from(descriptor)
}
