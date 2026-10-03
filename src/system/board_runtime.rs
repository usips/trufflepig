//! Runtime markers pin the board database and track recent router failures.

use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BoardDatabaseMarker {
    pub(super) database: PathBuf,
}

pub(crate) fn record_board_database(runtime: &Path, database: &Path) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    ensure!(
        database.is_absolute(),
        "invalid_options: board database path must be absolute"
    );
    fs::create_dir_all(runtime)?;
    let marker = runtime.join("board-backend.json");
    let bytes = serde_json::to_vec(&BoardDatabaseMarker {
        database: database.to_owned(),
    })?;
    // SAFETY: getuid has no preconditions and cannot fail.
    if board_database_marker_matches(&marker, &bytes, unsafe { libc::getuid() })? {
        return Ok(());
    }
    let temporary = runtime.join(format!("board-backend-{}.pending", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &marker)?;
        fs::File::open(runtime)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}

pub(super) fn board_database_marker_matches(
    marker: &Path,
    expected: &[u8],
    owner: u32,
) -> Result<bool> {
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    };
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(marker)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("board_unavailable: open database pin"),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.file_type().is_file()
            && metadata.uid() == owner
            && metadata.permissions().mode() & 0o7777 == 0o600,
        "board_unavailable: database pin must be a private regular file owned by the current user"
    );
    if metadata.len() != expected.len() as u64 {
        return Ok(false);
    }
    let mut bytes = Vec::with_capacity(expected.len());
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes == expected)
}

pub(crate) fn validate_board_database(runtime: &Path, database: &Path) -> Result<()> {
    let bytes = match fs::read(runtime.join("board-backend.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("board_unavailable: read database pin"),
    };
    let marker: BoardDatabaseMarker =
        serde_json::from_slice(&bytes).context("board_unavailable: invalid database pin")?;
    let equal = marker.database == database
        || marker
            .database
            .canonicalize()
            .ok()
            .zip(database.canonicalize().ok())
            .is_some_and(|(router, local)| router == local);
    ensure!(
        marker.database.is_absolute() && equal,
        "board_unavailable: local database {} differs from router database {}; restore the router database configuration",
        database.display(),
        marker.database.display()
    );
    Ok(())
}

const BOARD_UNAVAILABLE_TTL: Duration = Duration::from_secs(30);

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoardUnavailableMarker {
    unavailable_at: u64,
}

pub(crate) fn board_router_recently_unavailable(runtime: &Path, now: SystemTime) -> bool {
    let Ok(bytes) = fs::read(runtime.join("board-router-unavailable.json")) else {
        return false;
    };
    let Ok(marker) = serde_json::from_slice::<BoardUnavailableMarker>(&bytes) else {
        return false;
    };
    let recorded = SystemTime::UNIX_EPOCH + Duration::from_secs(marker.unavailable_at);
    now.duration_since(recorded)
        .is_ok_and(|age| age < BOARD_UNAVAILABLE_TTL)
}

pub(crate) fn mark_board_router_unavailable(runtime: &Path, now: SystemTime) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let marker = BoardUnavailableMarker {
        unavailable_at: now.duration_since(SystemTime::UNIX_EPOCH)?.as_secs(),
    };
    fs::create_dir_all(runtime)?;
    let temporary = runtime.join(format!(
        "board-unavailable-{}.pending",
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&marker)?)?;
        file.sync_all()?;
        fs::rename(&temporary, runtime.join("board-router-unavailable.json"))?;
        fs::File::open(runtime)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}

pub(crate) fn clear_board_router_unavailable(runtime: &Path) {
    let _ = fs::remove_file(runtime.join("board-router-unavailable.json"));
}
