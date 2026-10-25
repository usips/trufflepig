//! Loop guard for automatic restarts, persisted across router generations.

use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Automatic restarts allowed within [`RESTART_WINDOW`].
pub(super) const RESTART_LIMIT: usize = 3;
pub(super) const RESTART_WINDOW: Duration = Duration::from_secs(600);
/// File in the router runtime directory holding recent restart times.
pub(super) const RESTART_RECORD: &str = "router-restarts";

#[derive(Debug, Eq, PartialEq)]
pub(super) enum GuardVerdict {
    /// The restart was recorded; the router may exit.
    Admitted,
    /// `recent` restarts already happened; the oldest ages out after `retry_after`.
    Suppressed {
        recent: usize,
        retry_after: Duration,
    },
    /// The record could not be written, so the guard cannot bound a loop.
    Unrecorded(String),
}

/// Unix-second timestamps of recent automatic restarts, one per line.
#[derive(Debug)]
pub(super) struct RestartGuard {
    record: PathBuf,
}

impl RestartGuard {
    pub(super) fn new(record: PathBuf) -> Self {
        Self { record }
    }

    /// Restarts inside the window ending at `now`, oldest first.
    pub(super) fn recent(&self, now: SystemTime) -> Vec<u64> {
        let now = unix_seconds(now);
        let window = RESTART_WINDOW.as_secs();
        let mut recent: Vec<u64> = fs::read_to_string(&self.record)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse::<u64>().ok())
            .filter(|time| *time <= now && now - time < window)
            .collect();
        recent.sort_unstable();
        recent
    }

    /// Records a restart at `now` unless [`RESTART_LIMIT`] already happened in the window.
    pub(super) fn admit(&self, now: SystemTime) -> GuardVerdict {
        let mut recent = self.recent(now);
        let seconds = unix_seconds(now);
        if recent.len() >= RESTART_LIMIT {
            let oldest = recent[recent.len() - RESTART_LIMIT];
            let retry_after = RESTART_WINDOW.saturating_sub(Duration::from_secs(seconds - oldest));
            return GuardVerdict::Suppressed {
                recent: recent.len(),
                retry_after,
            };
        }
        recent.push(seconds);
        match self.write(&recent) {
            Ok(()) => GuardVerdict::Admitted,
            Err(error) => GuardVerdict::Unrecorded(format!(
                "cannot record restart in {}: {error}",
                self.record.display()
            )),
        }
    }

    fn write(&self, times: &[u64]) -> std::io::Result<()> {
        let staged = self.record.with_extension("staged");
        let mut file = fs::File::create(&staged)?;
        for time in times {
            writeln!(file, "{time}")?;
        }
        file.sync_all()?;
        fs::rename(staged, &self.record)
    }
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
