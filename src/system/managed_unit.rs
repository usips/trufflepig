//! Starts the router through the installed `trufflepig-system.service` user unit
//! when that unit owns the router runtime directory, so no detached router races
//! the unit for the daemon lock, and watches the unit while it starts.

use crate::coded_error::CodedContext;
use crate::process_capture::{self, CaptureError};
use crate::{background_process::BackgroundChild, query_deadline::QueryDeadline};
use anyhow::{Context, Result, anyhow};
use std::{
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

/// The user unit that `install.sh --systemd` renders.
pub(super) const UNIT_NAME: &str = "trufflepig-system.service";
const SYSTEMCTL_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// Least spacing between `systemctl show` reads while a unit start is awaited.
const UNIT_STATE_PROBE: Duration = Duration::from_millis(250);

/// How a start left the router: a detached child whose exit the poll loop
/// watches, or the installed unit, whose state the poll loop reads.
pub(super) enum RouterStart<'run> {
    Child(BackgroundChild),
    Unit(UnitWatch<'run>),
}

/// The unit's systemd state as `systemctl show` reports it.
pub(super) struct UnitState {
    pub(super) active: String,
    pub(super) restarts: u64,
}

/// Watches a started unit: `NRestarts` at start is the baseline that a later
/// increase, a crash loop between `activating` states, is measured against.
pub(super) struct UnitWatch<'run> {
    systemctl: Box<dyn FnMut(&[&str]) -> Result<String, String> + 'run>,
    restarts: Option<u64>,
    checked: Instant,
}

impl UnitWatch<'_> {
    /// The unit's state when systemd reports it failed or inactive, or restarted
    /// since the baseline. Reads at most once per `UNIT_STATE_PROBE`; a failed
    /// read is no answer, so the caller keeps polling.
    pub(super) fn failure(&mut self, now: Instant) -> Option<UnitState> {
        if now.saturating_duration_since(self.checked) < UNIT_STATE_PROBE {
            return None;
        }
        self.checked = now;
        let state = read_unit_state(&mut *self.systemctl).ok()?;
        let baseline = *self.restarts.get_or_insert(state.restarts);
        (matches!(state.active.as_str(), "failed" | "inactive") || state.restarts > baseline)
            .then_some(state)
    }
}

/// The error for a unit that failed or restarted before the router answered.
pub(super) fn did_not_start(state: &UnitState) -> anyhow::Error {
    anyhow!(coded!(
        SystemUnavailable,
        "{UNIT_NAME} did not start (ActiveState={}, NRestarts={}); see `journalctl --user -u {UNIT_NAME}`",
        state.active,
        state.restarts
    ))
}

/// Installed unit path: `$XDG_CONFIG_HOME/systemd/user/` when absolute, else
/// `$HOME/.config/systemd/user/`.
pub(super) fn unit_path_from(get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| {
        get(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let config = absolute("XDG_CONFIG_HOME")
        .or_else(|| absolute("HOME").map(|home| home.join(".config")))?;
    Some(config.join("systemd").join("user").join(UNIT_NAME))
}

/// Text of the installed unit; `None` when no unit file exists.
pub(super) fn installed_unit_text() -> Result<Option<String>> {
    let Some(path) = unit_path_from(|name| std::env::var_os(name)) else {
        return Ok(None);
    };
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_coded_context(|| {
            coded!(
                SystemUnavailable,
                "cannot read installed unit {}",
                path.display()
            )
        }),
    }
}

/// The unit's `TRUFFLEPIG_SYSTEM_DIR`, read from its `Environment=` lines the way
/// systemd does: the last assignment wins, an empty `Environment=` resets the list,
/// and `%%` is the escaped `%`.
pub(super) fn unit_system_dir(unit: &str) -> Option<PathBuf> {
    let mut system_dir = None;
    for line in unit.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "Environment" {
            continue;
        }
        if value.trim().is_empty() {
            system_dir = None;
            continue;
        }
        for word in environment_words(value) {
            if let Some(dir) = word.strip_prefix("TRUFFLEPIG_SYSTEM_DIR=") {
                system_dir = Some(PathBuf::from(dir.replace("%%", "%")));
            }
        }
    }
    system_dir
}

/// Splits an `Environment=` value into assignments: whitespace separates them,
/// and inside double quotes whitespace is kept and `\"` or `\\` escape a character.
fn environment_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quoted = false;
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        match character {
            '"' => {
                quoted = !quoted;
                word.get_or_insert_with(String::new);
            }
            '\\' if quoted => {
                if let Some(escaped) = characters.next() {
                    word.get_or_insert_with(String::new).push(escaped);
                }
            }
            separator if separator.is_whitespace() && !quoted => {
                words.extend(word.take());
            }
            other => word.get_or_insert_with(String::new).push(other),
        }
    }
    words.extend(word);
    words
}

/// Starts the router for `dir`: through the unit when it owns `dir`, else `spawn`.
/// A unit start reads `NRestarts` once afterwards as its watch baseline.
pub(super) fn start_router_with<'run>(
    unit: Option<&str>,
    dir: &Path,
    deadline: &QueryDeadline,
    mut run: impl FnMut(&[&str]) -> Result<String, String> + 'run,
    spawn: impl FnOnce() -> Result<BackgroundChild>,
) -> Result<RouterStart<'run>> {
    ensure_startup_deadline(deadline)?;
    let owned = unit.and_then(unit_system_dir);
    if owned.as_deref() != Some(dir) {
        ensure_startup_deadline(deadline)?;
        return spawn().map(RouterStart::Child);
    }
    // A unit that crash-looped is refused by systemd's start limit until reset;
    // the reset itself is best-effort, and the start below reports any failure.
    ensure_startup_deadline(deadline)?;
    let reset = run(&["--user", "reset-failed", UNIT_NAME]);
    ensure_startup_deadline(deadline)?;
    let _ = reset;
    let started = run(&["--user", "start", UNIT_NAME]);
    ensure_startup_deadline(deadline)?;
    started.map_err(|detail| {
        anyhow!(
            coded!(SystemUnavailable, "{UNIT_NAME} owns {} but `systemctl --user start` failed: {detail}; start it with `systemctl --user start {UNIT_NAME}`",
            dir.display())
        )
    })?;
    // A failed baseline read is not a failed start; the first watch read sets it.
    ensure_startup_deadline(deadline)?;
    let baseline = read_unit_state(&mut run).ok();
    ensure_startup_deadline(deadline)?;
    let restarts = baseline.map(|state| state.restarts);
    Ok(RouterStart::Unit(UnitWatch {
        systemctl: Box::new(run),
        restarts,
        checked: Instant::now(),
    }))
}

fn ensure_startup_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        return Err(anyhow!(coded!(
            SystemUnavailable,
            "router startup deadline expired"
        )));
    }
    Ok(())
}

/// Reads the unit's `ActiveState` and `NRestarts` with `systemctl show`.
fn read_unit_state(
    systemctl: &mut dyn FnMut(&[&str]) -> Result<String, String>,
) -> Result<UnitState> {
    let show = systemctl(&["--user", "show", "-p", "ActiveState,NRestarts", UNIT_NAME])
        .map_err(|detail| anyhow!("systemctl show failed: {detail}"))?;
    parse_unit_state(&show)
}

/// Parses `Key=value` lines in any order; other properties are ignored.
pub(super) fn parse_unit_state(show: &str) -> Result<UnitState> {
    let mut active = None;
    let mut restarts: Option<u64> = None;
    for line in show.lines() {
        match line.split_once('=') {
            Some(("ActiveState", value)) => active = Some(value.trim().to_owned()),
            Some(("NRestarts", value)) => restarts = value.trim().parse().ok(),
            _ => {}
        }
    }
    Ok(UnitState {
        active: active.context("systemctl show omitted ActiveState")?,
        restarts: restarts.context("systemctl show omitted a numeric NRestarts")?,
    })
}

/// Runs `systemctl` with `args` and returns its stdout; a failure carries stderr,
/// or the spawn error.
pub(super) fn run_systemctl(args: &[&str], deadline: &QueryDeadline) -> Result<String, String> {
    let mut command = Command::new("systemctl");
    command.args(args);
    run_command_until(&mut command, deadline)
}

/// Runs one owned subprocess, draining both pipes while bounding its wait.
pub(super) fn run_command_until(
    command: &mut Command,
    deadline: &QueryDeadline,
) -> Result<String, String> {
    let captured = process_capture::capture(command, None, deadline, SYSTEMCTL_OUTPUT_LIMIT)
        .map_err(|error| match error {
            CaptureError::TimedOut => "startup deadline expired".to_owned(),
            other => other.to_string(),
        })?;
    let stdout = String::from_utf8_lossy(&captured.stdout).into_owned();
    if captured.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&captured.stderr).trim().to_owned();
    Err(if stderr.is_empty() {
        captured.status.to_string()
    } else {
        stderr
    })
}
