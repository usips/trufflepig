use super::{BoardConfig, BoardGateway};
use crate::board::board_protocol::BoardErrorCode;
use anyhow::Result;
use std::{
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
};

/// Uses a nonblocking Unix connect so the diagnostic cannot stall on a full backlog.
pub(super) fn probe_socket(path: &Path) -> io::Result<()> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: Every field in sockaddr_un is an integer or byte array, so zero is valid.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= std::mem::size_of_val(&address.sun_path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "router socket path is too long",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path.iter_mut().zip(bytes) {
        *target = *source as libc::c_char;
    }
    let raw_fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: A successful socket call returns a fresh descriptor owned by this probe.
    let socket = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    // SAFETY: address is initialized and remains alive for the full connect call.
    let connected = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if connected == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(code) if code == libc::EINPROGRESS || code == libc::EALREADY || code == libc::EAGAIN
    ) {
        return Ok(());
    }
    Err(error)
}

pub(super) fn denied_router_socket(
    gateway: &mut dyn BoardGateway,
    runtime: Option<&Path>,
    load: &mut dyn FnMut() -> Result<BoardConfig>,
) -> Result<Option<anyhow::Error>> {
    let Some(runtime) = runtime else {
        return Ok(None);
    };
    let path = runtime.join(crate::daemon::SOCKET_NAME);
    let Err(error) = gateway.probe_socket(&path) else {
        return Ok(None);
    };
    if error.kind() != std::io::ErrorKind::PermissionDenied {
        return Ok(None);
    }
    let config = load()?;
    Ok(Some(anyhow::anyhow!(
        concat!(
            "board_unavailable: permission denied creating a client AF_UNIX socket or ",
            "connecting to router socket {} ({}); allow the sandbox to create AF_UNIX ",
            "client sockets and connect to that path; connection also needs parent-directory ",
            "traversal and socket access for the current Unix user; local database fallback ",
            "is {} and needs file read access plus parent-directory traversal for queries, ",
            "and write access to the file and parent directory for mutations; the client ",
            "keeps its current Unix identity and configured board actor"
        ),
        path.display(),
        error,
        config.db_path.display()
    )))
}

pub(super) fn is_storage_access_failure(error: &anyhow::Error) -> bool {
    if BoardErrorCode::from_error(error) != Some(BoardErrorCode::BoardUnavailable) {
        return false;
    }
    error.chain().any(|cause| {
        let detail = cause.to_string().to_ascii_lowercase();
        [
            "readonly database",
            "read-only database",
            "unable to open database",
            "database file is not writable",
            "database directory is not writable",
            "permission denied",
            "operation not permitted",
        ]
        .iter()
        .any(|marker| detail.contains(marker))
    })
}

pub(super) fn storage_access_advice(
    error: anyhow::Error,
    config: &BoardConfig,
    runtime: Option<&Path>,
) -> anyhow::Error {
    let socket = router_socket_path(runtime)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unavailable (no system runtime directory)".to_owned());
    anyhow::anyhow!(
        concat!(
            "{error:#}; local SQLite database {} could not be opened or written; check the ",
            "configured path and file type, parent-directory traversal, and permissions; ",
            "queries need database read access, while mutations also need database and ",
            "parent-directory write access; using router socket {} requires sandbox ",
            "permission to create a client AF_UNIX socket and connect to that path; ",
            "connection also needs directory traversal and socket access for the current ",
            "Unix user; keep the current Unix identity and configured board actor"
        ),
        config.db_path.display(),
        socket,
        error = error
    )
}

fn router_socket_path(runtime: Option<&Path>) -> Option<PathBuf> {
    runtime
        .map(Path::to_path_buf)
        .or_else(crate::system::dir)
        .map(|runtime| runtime.join(crate::daemon::SOCKET_NAME))
}
