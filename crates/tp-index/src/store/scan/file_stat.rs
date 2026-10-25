//! Filesystem identity recorded per published file, so a pass reuses a file's
//! published revision instead of reading it while this identity is unchanged
//! ([coherent publication](../../../docs/index-contract.md#coherent-publication)).
use rusqlite::Row;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::time::{SystemTime, UNIX_EPOCH};

/// A timestamp this close to a pass start may share its clock tick with a later
/// write, so its stat is not recorded (racy timestamps, as in Git's index).
const RACY_MARGIN_NS: i64 = 2_000_000_000;

#[cfg(test)]
thread_local! {
    /// Test clock: added to the pass start so fresh test files can be safely old.
    pub(in crate::store) static TEST_PASS_START_SKEW_NS: std::cell::Cell<i64> =
        const { std::cell::Cell::new(0) };
}

/// The stat fields that change whenever a file's bytes may have changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::store) struct FileStat {
    size: u64,
    mtime_ns: i64,
    ctime_ns: i64,
    inode: u64,
    device: u64,
}

impl FileStat {
    pub(in crate::store) fn of(metadata: &Metadata) -> Self {
        Self {
            size: metadata.size(),
            mtime_ns: metadata
                .mtime()
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.mtime_nsec()),
            ctime_ns: metadata
                .ctime()
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.ctime_nsec()),
            inode: metadata.ino(),
            device: metadata.dev(),
        }
    }

    /// Whether a modification or status change is not safely older than `pass_start`.
    pub(in crate::store) fn is_racy(&self, pass_start: PassStart) -> bool {
        self.mtime_ns.max(self.ctime_ns) > pass_start.0.saturating_sub(RACY_MARGIN_NS)
    }

    /// Reads the five nullable `files.stat_*` columns starting at `first`;
    /// any null means no recorded stat.
    pub(in crate::store) fn from_row(
        row: &Row<'_>,
        first: usize,
    ) -> rusqlite::Result<Option<Self>> {
        let size: Option<i64> = row.get(first)?;
        let mtime_ns: Option<i64> = row.get(first + 1)?;
        let ctime_ns: Option<i64> = row.get(first + 2)?;
        let inode: Option<i64> = row.get(first + 3)?;
        let device: Option<i64> = row.get(first + 4)?;
        Ok(match (size, mtime_ns, ctime_ns, inode, device) {
            (Some(size), Some(mtime_ns), Some(ctime_ns), Some(inode), Some(device)) => Some(Self {
                size: size as u64,
                mtime_ns,
                ctime_ns,
                inode: inode as u64,
                device: device as u64,
            }),
            _ => None,
        })
    }

    /// The `files.stat_*` column values; unsigned fields keep their bit pattern.
    pub(in crate::store) fn columns(stat: Option<Self>) -> [Option<i64>; 5] {
        match stat {
            Some(stat) => [
                Some(stat.size as i64),
                Some(stat.mtime_ns),
                Some(stat.ctime_ns),
                Some(stat.inode as i64),
                Some(stat.device as i64),
            ],
            None => [None; 5],
        }
    }
}

/// Wall-clock nanoseconds taken before a pass observes any file.
#[derive(Clone, Copy, Debug)]
pub(in crate::store) struct PassStart(i64);

impl PassStart {
    pub(in crate::store) fn now() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX)
            });
        #[cfg(test)]
        let now = now.saturating_add(TEST_PASS_START_SKEW_NS.get());
        Self(now)
    }
}
