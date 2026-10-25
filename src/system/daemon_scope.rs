//! Places router-spawned daemons in a low-weight systemd slice.
//! A supervised router passes the slice and its own cgroup to each child; the
//! child, still in that cgroup, asks the user manager for a transient scope bound
//! to the router unit. Failure leaves it in place. Contract: `docs/router-lifecycle.md`.

use std::{
    ffi::OsString,
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Slice the unit names for daemons; set in the router unit's environment.
pub(crate) const DAEMON_SLICE_ENV: &str = "TRUFFLEPIG_DAEMON_SLICE";
/// The router's cgroup, set only on processes the router spawns.
pub(crate) const SCOPE_FROM_ENV: &str = "TRUFFLEPIG_DAEMON_SCOPE_FROM";
const BUSCTL_WAIT: Duration = Duration::from_secs(1);
const MOVE_WAIT: Duration = Duration::from_millis(500);

/// What a supervised router hands its children.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DaemonPlacement {
    slice: String,
    router_cgroup: String,
}

impl DaemonPlacement {
    /// Placement for a router whose unit names `slice` and who runs in `router_cgroup`.
    pub(crate) fn new(slice: Option<OsString>, router_cgroup: Option<String>) -> Option<Self> {
        let slice = slice?.into_string().ok()?;
        let router_cgroup = router_cgroup?;
        (valid_slice(&slice) && bound_unit(&router_cgroup).is_some()).then_some(Self {
            slice,
            router_cgroup,
        })
    }

    /// Adds the placement to a child command.
    pub(crate) fn apply(&self, command: &mut Command) {
        command
            .env(DAEMON_SLICE_ENV, &self.slice)
            .env(SCOPE_FROM_ENV, &self.router_cgroup);
    }
}

/// The scope a child requests: its name, slice, and the unit it binds to.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ScopeRequest<'a> {
    pid: u32,
    slice: &'a str,
    unit: &'a str,
}

impl<'a> ScopeRequest<'a> {
    /// A request only while the child is still in the router's cgroup, so
    /// grandchildren already inside a daemon scope stay with their parent.
    pub(crate) fn new(pid: u32, slice: &'a str, from: &'a str, current: &str) -> Option<Self> {
        (current == from && valid_slice(slice)).then_some(())?;
        Some(Self {
            pid,
            slice,
            unit: bound_unit(from)?,
        })
    }

    /// `busctl` arguments for `StartTransientUnit` with this process's PID.
    pub(crate) fn busctl_args(&self) -> Vec<String> {
        let pid = self.pid.to_string();
        let words = [
            "--user",
            "--timeout=1",
            "call",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "StartTransientUnit",
            "ssa(sv)a(sa(sv))",
        ];
        let mut args = Vec::with_capacity(words.len() + 24);
        args.extend(words.map(str::to_owned));
        args.push(format!("trufflepig-daemon-{pid}.scope"));
        args.push("fail".into());
        args.push("5".into());
        args.extend(["Description", "s", "trufflepig background daemon"].map(str::to_owned));
        args.extend(["PIDs", "au", "1"].map(str::to_owned));
        args.push(pid);
        args.extend(["Slice", "s", self.slice].map(str::to_owned));
        args.extend(["BindsTo", "as", "1", self.unit].map(str::to_owned));
        args.extend(["CollectMode", "s", "inactive-or-failed"].map(str::to_owned));
        args.push("0".into());
        args
    }
}

/// The unified-hierarchy cgroup of the calling process.
pub(crate) fn own_cgroup() -> Option<String> {
    cgroup_path(&fs::read_to_string("/proc/self/cgroup").ok()?)
}

fn cgroup_path(proc_cgroup: &str) -> Option<String> {
    proc_cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .filter(|path| path.starts_with('/'))
        .map(str::to_owned)
}

/// The service unit a cgroup path ends in.
fn bound_unit(cgroup: &str) -> Option<&str> {
    let unit = Path::new(cgroup).file_name()?.to_str()?;
    (unit.ends_with(".service") && unit.len() > ".service".len()).then_some(unit)
}

fn valid_slice(slice: &str) -> bool {
    slice.len() <= 128
        && slice.len() > ".slice".len()
        && slice.ends_with(".slice")
        && slice
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b":_.-".contains(&byte))
}

/// Moves a router-spawned daemon into its slice before it starts any work.
/// Best effort: without the environment, systemd, or `busctl`, nothing changes.
pub(crate) fn enter_from_environment() {
    let (Some(slice), Some(from)) = (
        std::env::var(DAEMON_SLICE_ENV).ok(),
        std::env::var(SCOPE_FROM_ENV).ok(),
    ) else {
        return;
    };
    let Some(current) = own_cgroup() else {
        return;
    };
    let Some(request) = ScopeRequest::new(std::process::id(), &slice, &from, &current) else {
        return;
    };
    if !run_bounded(Command::new("busctl").args(request.busctl_args())) {
        return;
    }
    // The scope starts asynchronously; wait briefly so later children inherit it.
    let deadline = Instant::now() + MOVE_WAIT;
    while own_cgroup().as_deref() == Some(from.as_str()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run_bounded(command: &mut Command) -> bool {
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + BUSCTL_WAIT + Duration::from_millis(250);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests;
