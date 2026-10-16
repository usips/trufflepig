use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const SYSTEM_DIR: &str = "/run/user/1000/trufflepig/system";

/// Unit text shaped as `install.sh --systemd` renders it: JSON-quoted
/// `Environment=` lines, one per assignment.
fn rendered_unit(system_dir: &str) -> String {
    format!(
        "[Service]\n\
         Environment=\"TRUFFLEPIG_BOARD_DB=/home/josh/.local/share/trufflepig/board.sqlite3\"\n\
         Environment=\"TRUFFLEPIG_SYSTEM_DIR={system_dir}\"\n\
         Environment=\"TRUFFLEPIG_SPOOL_DIR=/home/josh/.cache/codex-tmp/trufflepig-agent/spool\"\n\
         Type=simple\n\
         ExecStart=\"/home/josh/.cargo/bin/trufflepig\" system-serve\n\
         Restart=on-failure\n"
    )
}

/// Fake `systemctl --user`: `reset-failed` and `start` succeed, and each `show`
/// answers the next scripted reply, the last reply repeating.
pub(super) struct FakeSystemctl {
    shows: RefCell<VecDeque<Result<String, String>>>,
    calls: RefCell<Vec<String>>,
}

impl FakeSystemctl {
    pub(super) fn new(shows: impl IntoIterator<Item = Result<&'static str, &'static str>>) -> Self {
        Self {
            shows: RefCell::new(
                shows
                    .into_iter()
                    .map(|reply| reply.map(str::to_owned).map_err(str::to_owned))
                    .collect(),
            ),
            calls: RefCell::new(Vec::new()),
        }
    }

    pub(super) fn run(&self, args: &[&str]) -> Result<String, String> {
        self.calls.borrow_mut().push(args.join(" "));
        if !args.contains(&"show") {
            return Ok(String::new());
        }
        let mut shows = self.shows.borrow_mut();
        let reply = if shows.len() > 1 {
            shows.pop_front()
        } else {
            shows.front().cloned()
        };
        reply.expect("a scripted show reply")
    }

    pub(super) fn show_count(&self) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|call| call.contains(" show "))
            .count()
    }
}

#[test]
fn unit_system_dir_reads_quoted_and_unquoted_assignments() {
    assert_eq!(
        managed_unit::unit_system_dir(&rendered_unit(SYSTEM_DIR)),
        Some(PathBuf::from(SYSTEM_DIR))
    );
    let unquoted = "[Service]\nEnvironment=TRUFFLEPIG_BOARD_DB=/db TRUFFLEPIG_SYSTEM_DIR=/run/a\n";
    assert_eq!(
        managed_unit::unit_system_dir(unquoted),
        Some(PathBuf::from("/run/a"))
    );
}

#[test]
fn unit_system_dir_unescapes_percent_specifiers_as_systemd_does() {
    let unit = rendered_unit("/run/user/1000/sys%%dir");
    assert_eq!(
        managed_unit::unit_system_dir(&unit),
        Some(PathBuf::from("/run/user/1000/sys%dir"))
    );
}

#[test]
fn unit_system_dir_takes_the_last_assignment_and_empty_resets_the_list() {
    let later =
        "Environment=\"TRUFFLEPIG_SYSTEM_DIR=/one\"\nEnvironment=\"TRUFFLEPIG_SYSTEM_DIR=/two\"\n";
    assert_eq!(
        managed_unit::unit_system_dir(later),
        Some(PathBuf::from("/two"))
    );
    let reset = format!("{later}Environment=\nEnvironment=\"TRUFFLEPIG_BOARD_DB=/db\"\n");
    assert_eq!(managed_unit::unit_system_dir(&reset), None);
    assert_eq!(
        managed_unit::unit_system_dir("[Service]\nType=simple\n"),
        None
    );
}

#[test]
fn unit_path_prefers_an_absolute_xdg_config_home_then_home() {
    let unit = managed_unit::UNIT_NAME;
    assert_eq!(
        managed_unit::unit_path_from(lookup(&[
            ("XDG_CONFIG_HOME", "/xdg"),
            ("HOME", "/home/josh"),
        ])),
        Some(PathBuf::from(format!("/xdg/systemd/user/{unit}")))
    );
    assert_eq!(
        managed_unit::unit_path_from(lookup(&[
            ("XDG_CONFIG_HOME", "relative/config"),
            ("HOME", "/home/josh"),
        ])),
        Some(PathBuf::from(format!(
            "/home/josh/.config/systemd/user/{unit}"
        )))
    );
    assert_eq!(managed_unit::unit_path_from(lookup(&[])), None);
}

#[test]
fn unit_state_parses_key_value_lines_in_any_order() {
    let state = managed_unit::parse_unit_state("NRestarts=3\nActiveState=activating\n").unwrap();
    assert_eq!((state.active.as_str(), state.restarts), ("activating", 3));
    assert!(
        managed_unit::parse_unit_state("ActiveState=failed\n").is_err(),
        "NRestarts is required"
    );
    assert!(
        managed_unit::parse_unit_state("NRestarts=1\n").is_err(),
        "ActiveState is required"
    );
}

#[test]
fn owned_unit_starts_through_systemd_without_a_detached_spawn() {
    let unit = rendered_unit(SYSTEM_DIR);
    let commands = RefCell::new(Vec::new());
    let started = managed_unit::start_router_with(
        Some(&unit),
        Path::new(SYSTEM_DIR),
        &QueryDeadline::after(Duration::from_secs(5)),
        |args| {
            commands.borrow_mut().push(args.join(" "));
            if args.contains(&"reset-failed") {
                Err("unit is not in a failed state".into())
            } else if args.contains(&"show") {
                Ok("ActiveState=activating\nNRestarts=0\n".into())
            } else {
                Ok(String::new())
            }
        },
        || panic!("a router owned by the unit must not spawn detached"),
    )
    .unwrap();
    assert!(
        matches!(started, managed_unit::RouterStart::Unit(_)),
        "systemd owns the started router"
    );
    assert_eq!(
        *commands.borrow(),
        [
            "--user reset-failed trufflepig-system.service",
            "--user start trufflepig-system.service",
            "--user show -p ActiveState,NRestarts trufflepig-system.service",
        ]
    );
}

#[test]
fn unit_watch_reads_once_per_probe_and_reports_failure_or_restart() {
    let unit = rendered_unit(SYSTEM_DIR);
    let systemctl = FakeSystemctl::new([
        Ok("ActiveState=activating\nNRestarts=2"),
        Ok("ActiveState=failed\nNRestarts=2"),
        Ok("ActiveState=activating\nNRestarts=3"),
    ]);
    let started = Instant::now();
    let managed_unit::RouterStart::Unit(mut watch) = managed_unit::start_router_with(
        Some(&unit),
        Path::new(SYSTEM_DIR),
        &QueryDeadline::after(Duration::from_secs(5)),
        |args| systemctl.run(args),
        || panic!("the unit owns this router"),
    )
    .unwrap() else {
        panic!("the unit owns this router");
    };
    assert!(
        watch
            .failure(started + Duration::from_millis(100))
            .is_none(),
        "no read inside the probe interval"
    );
    assert_eq!(systemctl.show_count(), 1, "only the baseline read so far");
    let failed = watch
        .failure(started + Duration::from_millis(300))
        .expect("a failed unit is reported");
    assert_eq!((failed.active.as_str(), failed.restarts), ("failed", 2));
    let restarted = watch
        .failure(started + Duration::from_millis(600))
        .expect("a restart above the baseline is reported while activating");
    assert_eq!(
        (restarted.active.as_str(), restarted.restarts),
        ("activating", 3)
    );
}

#[test]
fn unit_naming_another_dir_leaves_the_router_to_a_detached_spawn() {
    let unit = rendered_unit("/run/user/1000/trufflepig/other");
    assert_detached_spawn(Some(&unit));
}

#[test]
fn absent_unit_leaves_the_router_to_a_detached_spawn() {
    assert_detached_spawn(None);
}

fn assert_detached_spawn(unit: Option<&str>) {
    let runner_called = Cell::new(false);
    let error = managed_unit::start_router_with(
        unit,
        Path::new(SYSTEM_DIR),
        &QueryDeadline::after(Duration::from_secs(5)),
        |_| {
            runner_called.set(true);
            Ok(String::new())
        },
        || Err(anyhow::anyhow!("detached spawn")),
    )
    .err()
    .expect("the detached spawn decides the start");
    assert!(
        !runner_called.get(),
        "systemctl must not run for a router the unit does not own"
    );
    assert_eq!(format!("{error:#}"), "detached spawn");
}

#[test]
fn failed_unit_start_is_a_typed_error_and_never_spawns_detached() {
    let unit = rendered_unit(SYSTEM_DIR);
    let spawn_called = Cell::new(false);
    let error = managed_unit::start_router_with(
        Some(&unit),
        Path::new(SYSTEM_DIR),
        &QueryDeadline::after(Duration::from_secs(5)),
        |args| {
            if args.contains(&"start") {
                Err("Failed to connect to bus: No medium found".into())
            } else {
                Ok(String::new())
            }
        },
        || {
            spawn_called.set(true);
            Err(anyhow::anyhow!("unexpected detached spawn"))
        },
    )
    .err()
    .expect("a failed unit start is an error");
    assert!(
        !spawn_called.get(),
        "a unit-owned router must not fall back to a detached spawn"
    );
    assert_eq!(
        format!("{error:#}"),
        format!(
            "system_unavailable: trufflepig-system.service owns {SYSTEM_DIR} but `systemctl --user start` failed: Failed to connect to bus: No medium found; start it with `systemctl --user start trufflepig-system.service`"
        )
    );
}

#[test]
fn managed_startup_does_not_start_after_reset_consumes_deadline() {
    let unit = rendered_unit(SYSTEM_DIR);
    let deadline = QueryDeadline::after(Duration::from_millis(100));
    let commands = RefCell::new(Vec::new());
    let error = managed_unit::start_router_with(
        Some(&unit),
        Path::new(SYSTEM_DIR),
        &deadline,
        |args| {
            commands.borrow_mut().push(args.join(" "));
            std::thread::sleep(Duration::from_millis(150));
            Err("reset failed after consuming the startup budget".into())
        },
        || panic!("an expired managed start must not fall back to a detached spawn"),
    )
    .err()
    .expect("an expired reset phase stops managed startup");

    assert!(format!("{error:#}").contains("startup deadline expired"));
    assert_eq!(
        *commands.borrow(),
        ["--user reset-failed trufflepig-system.service"],
        "reset failure is ignored only while the deadline remains live"
    );
}

#[test]
fn managed_startup_does_not_show_after_start_consumes_deadline() {
    let unit = rendered_unit(SYSTEM_DIR);
    let deadline = QueryDeadline::after(Duration::from_millis(100));
    let commands = RefCell::new(Vec::new());
    let error = managed_unit::start_router_with(
        Some(&unit),
        Path::new(SYSTEM_DIR),
        &deadline,
        |args| {
            commands.borrow_mut().push(args.join(" "));
            if args.contains(&"start") {
                std::thread::sleep(Duration::from_millis(150));
            }
            Ok(String::new())
        },
        || panic!("an expired managed start must not fall back to a detached spawn"),
    )
    .err()
    .expect("an expired start phase stops managed startup");

    assert!(format!("{error:#}").contains("startup deadline expired"));
    assert_eq!(
        *commands.borrow(),
        [
            "--user reset-failed trufflepig-system.service",
            "--user start trufflepig-system.service",
        ],
        "systemctl show must not start after the deadline"
    );
}

#[test]
fn systemctl_command_drains_output_while_waiting() {
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2",
    ]);
    let output = managed_unit::run_command_until(
        &mut command,
        &QueryDeadline::after(Duration::from_secs(5)),
    )
    .expect("the command exits after both pipes are drained");

    assert_eq!(output.len(), 131072);
}

#[test]
fn systemctl_command_bounds_descendant_pipe_and_reaps_owned_child() {
    let scratch = crate::board::board_test_support::scratch("systemctl-deadline-");
    let pid_path = scratch.path().join("parent-pid");
    let marker_path = scratch.path().join("descendant-survived");
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "printf '%s' \"$$\" > \"$TRUFFLEPIG_TEST_PID_FILE\"; (sleep 0.3; printf alive > \"$TRUFFLEPIG_TEST_MARKER_FILE\") & exit 0",
        ])
        .env("TRUFFLEPIG_TEST_PID_FILE", &pid_path)
        .env("TRUFFLEPIG_TEST_MARKER_FILE", &marker_path);
    let deadline = QueryDeadline::after(Duration::from_millis(100));
    let started = Instant::now();
    let error = managed_unit::run_command_until(&mut command, &deadline)
        .expect_err("the descendant retains the owned process's output pipes");

    assert!(error.contains("startup deadline expired"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded descendant cleanup took {:?}",
        started.elapsed()
    );
    let child_pid = std::fs::read_to_string(pid_path)
        .expect("the owned child recorded its process id")
        .parse::<libc::pid_t>()
        .expect("the child process id is numeric");
    let mut status = 0;
    // SAFETY: this test targets the exact child PID, which the command helper
    // must already have waited for before returning from the timeout.
    let waited = unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };
    assert_eq!(waited, -1, "the child is no longer waitable by this test");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD),
        "the helper reaped its owned child"
    );
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !marker_path.exists(),
        "the descendant was killed with the process group before it could write"
    );
}

#[test]
fn systemctl_command_kills_and_reaps_owned_child_when_deadline_expires() {
    let scratch = crate::board::board_test_support::scratch("systemctl-child-");
    let pid_path = scratch.path().join("pid");
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "printf '%s' \"$$\" > \"$TRUFFLEPIG_TEST_PID_FILE\"; exec sleep 5",
        ])
        .env("TRUFFLEPIG_TEST_PID_FILE", &pid_path);
    let deadline = QueryDeadline::after(Duration::from_millis(100));
    let started = Instant::now();
    let error = managed_unit::run_command_until(&mut command, &deadline)
        .expect_err("the owned process exceeds the startup deadline");

    assert!(error.contains("startup deadline expired"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded command wait took {:?}",
        started.elapsed()
    );
    let child_pid = std::fs::read_to_string(pid_path)
        .expect("the child recorded its process id")
        .parse::<libc::pid_t>()
        .expect("the child process id is numeric");
    let mut status = 0;
    // SAFETY: this test targets the exact child PID, which the command helper
    // must already have waited for before returning from the timeout.
    let waited = unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };
    assert_eq!(waited, -1, "the child is no longer waitable by this test");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD),
        "the helper reaped its owned child"
    );
}
