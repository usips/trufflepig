use super::*;
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant, UNIX_EPOCH},
};

fn environment(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let values: HashMap<String, OsString> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |name| values.get(name).cloned()
}

#[test]
fn systemd_supervision_needs_the_unit_marker_and_an_invocation() {
    assert!(supervised_by_systemd(environment(&[
        (SUPERVISOR_ENV, "systemd"),
        ("INVOCATION_ID", "0f4c2b8e"),
    ])));
    for pairs in [
        &[][..],
        &[("INVOCATION_ID", "0f4c2b8e")][..],
        &[(SUPERVISOR_ENV, "systemd")][..],
        &[(SUPERVISOR_ENV, "systemd"), ("INVOCATION_ID", "")][..],
        &[(SUPERVISOR_ENV, "detached"), ("INVOCATION_ID", "0f4c2b8e")][..],
    ] {
        assert!(!supervised_by_systemd(environment(pairs)), "{pairs:?}");
    }
}

fn write_binary(path: &Path, body: &[u8]) {
    let staged = path.with_extension("staged");
    let mut bytes = b"\x7fELF".to_vec();
    bytes.extend_from_slice(body);
    fs::write(&staged, bytes).unwrap();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
    // `cargo install` replaces the path by rename, unlinking the running inode.
    fs::rename(&staged, path).unwrap();
}

struct WatchFixture {
    _directory: tempfile::TempDir,
    executable: PathBuf,
    record: PathBuf,
    watch: BinaryWatch,
}

fn watch_fixture(settle_time: Duration) -> WatchFixture {
    let directory = tp_core::test_fixtures::scratch("router-lifecycle-");
    let executable = directory.path().join("trufflepig");
    write_binary(&executable, b"old");
    let record = directory.path().join(RESTART_RECORD);
    let watch = BinaryWatch {
        executable: executable.clone(),
        running: FileIdentity::of(&executable).unwrap(),
        settle_time,
        settle: SettleTracker::default(),
        guard: RestartGuard::new(record.clone()),
        next_check: None,
        phase: RestartPhase::Current,
    };
    WatchFixture {
        _directory: directory,
        executable,
        record,
        watch,
    }
}

fn wall(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

#[test]
fn settle_requires_one_launchable_sample_for_the_whole_settle_time() {
    let directory = tp_core::test_fixtures::scratch("binary-settle-");
    let path = directory.path().join("trufflepig");
    write_binary(&path, b"first");
    let settle = Duration::from_secs(30);
    let start = Instant::now();
    let mut tracker = SettleTracker::default();
    let first = BinarySample::of(&path).unwrap();
    assert!(first.launchable);
    assert!(!tracker.observe(Some(&first), start, settle));
    assert!(!tracker.observe(Some(&first), start + Duration::from_secs(29), settle));
    assert!(tracker.observe(Some(&first), start + settle, settle));

    write_binary(&path, b"second, still being installed");
    let second = BinarySample::of(&path).unwrap();
    assert!(
        !tracker.observe(Some(&second), start + Duration::from_secs(31), settle),
        "a changed file restarts the settle time"
    );
    assert!(tracker.observe(Some(&second), start + Duration::from_secs(61), settle));

    fs::write(&path, b"#!/bin/sh\n").unwrap();
    let script = BinarySample::of(&path).unwrap();
    assert!(!script.launchable, "only an ELF file is launchable");
    assert!(!tracker.observe(Some(&script), start + Duration::from_secs(200), settle));
    assert!(!tracker.observe(None, start + Duration::from_secs(300), settle));
}

#[test]
fn a_replaced_binary_restarts_only_after_it_settles() {
    let mut fixture = watch_fixture(Duration::from_secs(30));
    let start = Instant::now();
    assert!(fixture.watch.tick(start, wall(1_000)).is_none());
    assert_eq!(fixture.watch.phase, RestartPhase::Current);

    write_binary(&fixture.executable, b"new");
    let at = |seconds| start + Duration::from_secs(seconds);
    assert!(fixture.watch.tick(at(1), wall(1_001)).is_none());
    assert_eq!(fixture.watch.phase, RestartPhase::Settling);
    assert!(
        fixture.watch.tick(at(1), wall(1_001)).is_none(),
        "samples at most once per check interval"
    );
    assert!(fixture.watch.tick(at(30), wall(1_030)).is_none());
    let restart = fixture.watch.tick(at(31), wall(1_031)).expect("settled");
    assert_eq!(restart.executable, fixture.executable);
    assert!(restart.to_string().starts_with("router_restart: "));
    assert_eq!(fixture.watch.phase, RestartPhase::Exiting);
    assert!(fixture.watch.tick(at(40), wall(1_040)).is_none(), "once");
    assert_eq!(fs::read_to_string(&fixture.record).unwrap(), "1031\n");
}

#[test]
fn a_deleted_or_unlaunchable_binary_never_restarts() {
    let mut fixture = watch_fixture(Duration::ZERO);
    let start = Instant::now();
    fs::remove_file(&fixture.executable).unwrap();
    for second in 0..5 {
        let now = start + Duration::from_secs(second * 2);
        assert!(fixture.watch.tick(now, wall(2_000 + second)).is_none());
    }
    assert_eq!(fixture.watch.phase, RestartPhase::Settling);
    fs::write(&fixture.executable, b"partial download").unwrap();
    assert!(
        fixture
            .watch
            .tick(start + Duration::from_secs(20), wall(2_020))
            .is_none()
    );
    assert!(!fixture.record.exists());
}

#[test]
fn guard_allows_three_restarts_per_ten_minutes() {
    let directory = tp_core::test_fixtures::scratch("restart-guard-");
    let guard = RestartGuard::new(directory.path().join(RESTART_RECORD));
    for second in [100, 200, 300] {
        assert_eq!(guard.admit(wall(second)), GuardVerdict::Admitted);
    }
    assert_eq!(
        guard.admit(wall(400)),
        GuardVerdict::Suppressed {
            recent: 3,
            retry_after: Duration::from_secs(300),
        }
    );
    assert_eq!(guard.recent(wall(699)), vec![100, 200, 300]);
    assert_eq!(guard.admit(wall(700)), GuardVerdict::Admitted);
    assert_eq!(guard.recent(wall(700)), vec![200, 300, 700]);

    let unwritable = RestartGuard::new(directory.path().join("missing/dir").join(RESTART_RECORD));
    assert!(matches!(
        unwritable.admit(wall(100)),
        GuardVerdict::Unrecorded(_)
    ));
}

#[test]
fn a_full_guard_suppresses_restart_until_the_window_passes() {
    let mut fixture = watch_fixture(Duration::ZERO);
    fs::write(&fixture.record, "5000\n5100\n5200\n").unwrap();
    write_binary(&fixture.executable, b"new");
    let start = Instant::now();
    assert!(fixture.watch.tick(start, wall(5_300)).is_none());
    assert_eq!(fixture.watch.phase, RestartPhase::Suppressed { recent: 3 });
    let restart = fixture
        .watch
        .tick(start + Duration::from_secs(300), wall(5_600))
        .expect("the oldest restart aged out");
    assert_eq!(restart.executable, fixture.executable);
}

#[test]
fn restart_maps_to_the_dedicated_exit_status_through_context() {
    let restart = anyhow::Error::new(RouterRestart {
        executable: PathBuf::from("/bin/trufflepig"),
        settled: SETTLE,
    })
    .context("serve router");
    assert_eq!(exit_status(&restart), Some(ROUTER_RESTART_EXIT_STATUS));
    assert_eq!(exit_status(&anyhow::anyhow!("router failed")), None);
}

#[test]
fn detached_lifecycle_is_inert_and_children_lose_the_supervisor_marker() {
    let lifecycle = RouterLifecycle::default();
    assert!(lifecycle.tick(Instant::now(), SystemTime::now()).is_none());
    assert_eq!(
        lifecycle.status(),
        serde_json::json!({ "supervisor": "detached" })
    );
    let mut command = Command::new("true");
    command.env(SUPERVISOR_ENV, "systemd");
    lifecycle.prepare_child(&mut command);
    let removed = command
        .get_envs()
        .find(|(name, _)| *name == SUPERVISOR_ENV)
        .map(|(_, value)| value);
    assert_eq!(removed, Some(None), "the marker is removed for children");
    assert!(
        !command
            .get_envs()
            .any(|(name, _)| name == daemon_scope::SCOPE_FROM_ENV)
    );
}

#[test]
fn spawned_daemons_are_tracked_once_and_stopping_without_listeners_is_prompt() {
    let lifecycle = RouterLifecycle::default();
    let directory = tp_core::test_fixtures::scratch("spawned-daemons-");
    for _ in 0..3 {
        lifecycle.record_spawn(directory.path());
    }
    assert_eq!(lifecycle.spawned.lock().unwrap().len(), 1);
    let started = Instant::now();
    lifecycle.stop_spawned(Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(lifecycle.spawned.lock().unwrap().is_empty());
}

#[test]
fn deleted_suffix_is_stripped_from_the_watched_path() {
    assert_eq!(
        strip_deleted_suffix(PathBuf::from("/home/u/.cargo/bin/trufflepig (deleted)")),
        PathBuf::from("/home/u/.cargo/bin/trufflepig")
    );
    assert_eq!(
        strip_deleted_suffix(PathBuf::from("/bin/trufflepig")),
        PathBuf::from("/bin/trufflepig")
    );
}
