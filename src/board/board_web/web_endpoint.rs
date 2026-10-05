//! Secure discovery of the actual loopback listener, checked before printing links.
mod endpoint_probe;
mod listener_owner;
mod port_file;
#[cfg(test)]
mod tests;

use super::{
    http_wire,
    web_guard::{BoardWebToken, WebGuard},
};
use crate::board::{board_config::BoardConfig, board_ids::BoardRef, board_protocol::BOARD_API};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Instant,
};

const ENDPOINT_FILE: &str = "board-web.json";
const ENDPOINT_LIMIT: u64 = 16 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebEndpoint {
    api: u32,
    address: SocketAddr,
    database: PathBuf,
}

pub(super) fn publish(runtime: &Path, address: SocketAddr, database: &Path) -> Result<()> {
    ensure!(
        database.is_absolute(),
        "board_unavailable: database must be absolute"
    );
    fs::create_dir_all(runtime)?;
    let destination = runtime.join(ENDPOINT_FILE);
    match open_private(&destination) {
        Ok(_) => {}
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    let pending = runtime.join(format!(
        ".board-web-{}.pending",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&pending)?;
        let bytes = serde_json::to_vec(&WebEndpoint {
            api: BOARD_API,
            address,
            database: database.to_owned(),
        })?;
        ensure!(
            bytes.len() as u64 <= ENDPOINT_LIMIT,
            "board_unavailable: endpoint metadata too large"
        );
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&pending, destination)?;
        File::open(runtime)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(pending);
    result
}

/// Lenient read of the published listener address for advisory decisions
/// (the refusal message); strict checks stay on the publish/link path.
pub(super) fn read_published_address(runtime: &Path) -> Option<SocketAddr> {
    let bytes = fs::read(runtime.join(ENDPOINT_FILE)).ok()?;
    serde_json::from_slice::<WebEndpoint>(&bytes)
        .map(|endpoint| endpoint.address)
        .ok()
}

pub(super) fn published_origin(runtime: &Path) -> Option<String> {
    read_published_address(runtime).map(WebGuard::origin_for)
}

/// Bind `requested`, retaking the recorded port first when it asks for port 0;
/// a taken recorded port falls back to ephemeral with a one-line notice. Every
/// successful bind re-records its actual port for the next start.
pub(super) fn bind_listener(runtime: &Path, requested: SocketAddr) -> std::io::Result<TcpListener> {
    super::web_guard::require_loopback(requested)?;
    let persisted = (requested.port() == 0)
        .then(|| port_file::read(runtime))
        .flatten()
        .map(|port| SocketAddr::new(requested.ip(), port));
    let listener = match persisted {
        Some(candidate) => match TcpListener::bind(candidate) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                let fallback = TcpListener::bind(requested)?;
                eprintln!(
                    "board-serve: port {} in use; using {}",
                    candidate.port(),
                    fallback.local_addr()?.port()
                );
                fallback
            }
            Err(error) => return Err(error),
        },
        None => TcpListener::bind(requested)?,
    };
    port_file::record(runtime, listener.local_addr()?.port()).map_err(std::io::Error::other)?;
    Ok(listener)
}

/// Remove the descriptor only while it still names `address`; a foreign
/// rewrite, an absent file, or an unparsable file is left alone.
pub(super) fn remove_if_ours(path: &Path, address: SocketAddr) {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return,
    };
    let ours = serde_json::from_slice::<WebEndpoint>(&bytes)
        .is_ok_and(|endpoint| endpoint.address == address);
    if ours {
        let _ = fs::remove_file(path);
    }
}

/// Removes the published descriptor on drop while it still names our bound
/// address; board-serve owns one for its lifetime and its signal waiter
/// removes the same file before exiting.
pub(super) struct EndpointGuard {
    path: PathBuf,
    address: SocketAddr,
}

impl EndpointGuard {
    pub(super) fn arm(runtime: &Path, address: SocketAddr) -> Self {
        Self {
            path: runtime.join(ENDPOINT_FILE),
            address,
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for EndpointGuard {
    fn drop(&mut self) {
        remove_if_ours(&self.path, self.address);
    }
}

pub(super) fn link(target: Option<BoardRef>) -> Result<String> {
    let runtime = crate::system::dir().context("board_unavailable: no runtime directory")?;
    link_at(&runtime, &BoardConfig::load()?, target)
}

fn link_at(runtime: &Path, config: &BoardConfig, target: Option<BoardRef>) -> Result<String> {
    config.ensure_local()?;
    if let Some(target) = target {
        target.validate()?;
        ensure!(
            matches!(
                target,
                BoardRef::Plan(_) | BoardRef::Revision(_) | BoardRef::Entry(_)
            ),
            "invalid_reference: board web accepts P#, P#@#, or E#"
        );
    }
    let mut file = match open_private(&runtime.join(ENDPOINT_FILE)) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            anyhow::bail!("board_unavailable: board web is not listening; run board-serve");
        }
        Err(error) => return Err(error).context("board_unavailable: unsafe web endpoint"),
    };
    let mut bytes = Vec::with_capacity(512);
    Read::by_ref(&mut file)
        .take(ENDPOINT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= ENDPOINT_LIMIT,
        "board_unavailable: endpoint metadata too large"
    );
    let endpoint: WebEndpoint =
        serde_json::from_slice(&bytes).context("board_unavailable: invalid web endpoint")?;
    ensure!(
        endpoint.api == BOARD_API,
        "board_api_mismatch: restart board-serve"
    );
    ensure!(
        endpoint.database.is_absolute() && same_database(&endpoint.database, &config.db_path),
        "board_unavailable: running web database differs from configured database"
    );
    crate::system::validate_board_database(runtime, &config.db_path)?;
    let token = BoardWebToken::read_at(&runtime.join("board-web.token"))?;
    let guard = WebGuard::with_token(endpoint.address, token.clone())
        .context("board_unavailable: invalid web listener address")?;
    listener_owner::require_owned_listener(&endpoint.address)
        .and_then(|()| {
            endpoint_probe::probe(
                endpoint.address,
                &guard,
                Instant::now() + http_wire::REQUEST_TIMEOUT,
            )
        })
        .with_context(|| {
            format!(
                "board_unavailable: no verified listener at {}; run board-serve",
                guard.origin()
            )
        })?;
    let suffix = target.map_or_else(String::new, |target| format!("?ref={target}"));
    Ok(format!(
        "{}/{}#token={}",
        guard.origin(),
        suffix,
        token.expose()
    ))
}

fn same_database(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

fn open_private(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.mode() & 0o7777 == 0o600
        // SAFETY: geteuid has no preconditions and cannot fail.
        && metadata.uid() == unsafe { libc::geteuid() },
        "board_unavailable: endpoint must be an owned regular 0600 file"
    );
    Ok(file)
}
