//! Rotating local/LAN token file: O_NOFOLLOW, owner-only, atomically published.
//! Rotation writes a fresh token beside the destination and renames it over
//! any planted entry; reads never create files or mint secrets.

use super::super::board_web_secrets::{constant_time_equal, fill_random, hex_encode};
use anyhow::{Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use tp_core::coded_error::CodedContext;

const TOKEN_BYTES: usize = 64;

#[derive(Clone)]
pub(crate) struct BoardWebToken([u8; TOKEN_BYTES]);

impl BoardWebToken {
    /// Rotate to a fresh token; board-serve calls this on every start so a
    /// leaked token dies with the service that published it.
    pub(crate) fn rotate() -> Result<Self> {
        let directory = tp_core::runtime_paths::system_dir()
            .with_coded_context(|| coded!(BoardWebToken, "no runtime directory"))?;
        fs::create_dir_all(&directory)
            .with_coded_context(|| coded!(BoardWebToken, "create runtime directory"))?;
        Self::rotate_at(&directory.join("board-web.token"))
    }

    /// Read an existing private token without creating files or generating a secret.
    pub(crate) fn read_at(path: &Path) -> Result<Self> {
        read_token(
            open_existing(path)
                .with_coded_context(|| coded!(BoardWebToken, "open existing token"))?,
        )
    }

    /// Write a fresh token with the load-time O_NOFOLLOW/0600 discipline, then
    /// atomically rename it over any previous or planted destination entry.
    /// Existing unsafe or malformed entries are replaced, never written through.
    pub(crate) fn rotate_at(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let token = random_token()?;
        let mut pending = PendingTokenFile::create(parent)?;
        validate_file(&pending.file, false)?;
        pending
            .file
            .write_all(&token.0)
            .with_coded_context(|| coded!(BoardWebToken, "write token"))?;
        pending
            .file
            .sync_all()
            .with_coded_context(|| coded!(BoardWebToken, "sync token"))?;
        fs::rename(&pending.path, path)
            .with_coded_context(|| coded!(BoardWebToken, "publish rotated token"))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_coded_context(|| coded!(BoardWebToken, "sync token directory"))?;
        Ok(token)
    }

    pub(crate) fn expose(&self) -> &str {
        std::str::from_utf8(&self.0).expect("token is ASCII hexadecimal")
    }

    pub(crate) fn matches(&self, candidate: &str) -> bool {
        constant_time_equal(&self.0, candidate.as_bytes())
    }
}

struct PendingTokenFile {
    file: File,
    path: PathBuf,
}

impl PendingTokenFile {
    fn create(parent: &Path) -> Result<Self> {
        let name = format!(".board-web-token-{}.pending", uuid::Uuid::new_v4().simple());
        let path = parent.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&path)
            .with_coded_context(|| coded!(BoardWebToken, "create private token candidate"))?;
        Ok(Self { file, path })
    }
}

impl Drop for PendingTokenFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn open_existing(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
}

fn read_token(mut file: File) -> Result<BoardWebToken> {
    validate_file(&file, true)?;
    let mut bytes = [0; TOKEN_BYTES];
    file.read_exact(&mut bytes)
        .with_coded_context(|| coded!(BoardWebToken, "read token"))?;
    let mut extra = [0];
    ensure!(
        file.read(&mut extra)? == 0,
        coded!(BoardWebToken, "token length changed")
    );
    ensure!(
        bytes.iter().all(u8::is_ascii_hexdigit),
        coded!(BoardWebToken, "invalid token bytes")
    );
    Ok(BoardWebToken(bytes))
}

fn validate_file(file: &File, existing: bool) -> Result<()> {
    let metadata = file
        .metadata()
        .with_coded_context(|| coded!(BoardWebToken, "inspect token"))?;
    ensure!(
        metadata.file_type().is_file(),
        coded!(BoardWebToken, "token is not a regular file")
    );
    // SAFETY: geteuid has no preconditions and cannot fail.
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        coded!(BoardWebToken, "wrong token owner")
    );
    ensure!(
        metadata.mode() & 0o7777 == 0o600,
        coded!(BoardWebToken, "token mode must be 0600")
    );
    if existing {
        ensure!(
            metadata.len() == TOKEN_BYTES as u64,
            coded!(BoardWebToken, "invalid token length")
        );
    }
    Ok(())
}

fn random_token() -> Result<BoardWebToken> {
    let mut random = [0; TOKEN_BYTES / 2];
    fill_random(&mut random)
        .with_coded_context(|| coded!(BoardWebToken, "read operating-system random source"))?;
    let mut bytes = [0; TOKEN_BYTES];
    bytes.copy_from_slice(hex_encode(&random).as_bytes());
    Ok(BoardWebToken(bytes))
}
