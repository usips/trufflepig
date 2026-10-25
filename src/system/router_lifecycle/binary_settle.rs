//! Replacement detection and settle tracking for the router's executable path.

use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    time::{Duration, Instant},
};

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";

/// Device and inode of one executable file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    pub(super) fn of(path: &Path) -> Option<Self> {
        fs::metadata(path).ok().map(|metadata| Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

/// What the executable path holds now: identity, size, mtime, and launchability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BinarySample {
    pub(super) identity: FileIdentity,
    length: u64,
    modified_ns: i128,
    /// A regular, executable file that starts with the ELF magic.
    pub(super) launchable: bool,
}

impl BinarySample {
    /// Samples `path`; `None` when nothing is there.
    pub(super) fn of(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        let launchable = metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
            && starts_with_elf_magic(path);
        Some(Self {
            identity: FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            length: metadata.len(),
            modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
            launchable,
        })
    }
}

fn starts_with_elf_magic(path: &Path) -> bool {
    let mut magic = [0_u8; 4];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok_and(|()| &magic == ELF_MAGIC)
}

/// A replacement is settled once the same launchable sample has been observed
/// for the whole settle time; any change or unlaunchable sample restarts it.
#[derive(Debug, Default)]
pub(super) struct SettleTracker {
    stable_since: Option<(BinarySample, Instant)>,
}

impl SettleTracker {
    pub(super) fn observe(
        &mut self,
        sample: Option<&BinarySample>,
        now: Instant,
        settle: Duration,
    ) -> bool {
        let Some(sample) = sample.filter(|sample| sample.launchable) else {
            self.stable_since = None;
            return false;
        };
        match &self.stable_since {
            Some((previous, since)) if previous == sample => {
                now.saturating_duration_since(*since) >= settle
            }
            _ => {
                self.stable_since = Some((sample.clone(), now));
                settle.is_zero()
            }
        }
    }

    pub(super) fn reset(&mut self) {
        self.stable_since = None;
    }
}
