use super::{
    BackgroundOwner, ReplacedExecutable, background_executable_replaced,
    current_background_executable, spawn_background, spawn_background_for,
    validate_background_executable,
};
use std::{
    io,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn deleted_executable_has_typed_owner_specific_recovery_advice() {
    let executable = PathBuf::from("/opt/trufflepig (deleted)");
    let cache = PathBuf::from("/cache/root");
    let owners = [
        (
            BackgroundOwner::Command,
            "the command started",
            "rerun the command with the current trufflepig binary",
            &["trufflepig-system.service", "trufflepig system stop"][..],
        ),
        (
            BackgroundOwner::SystemRouter,
            "the system router started",
            "systemctl --user restart trufflepig-system.service",
            &[
                "root daemon for this cache",
                "workspace coordinator for this cache",
            ][..],
        ),
        (
            BackgroundOwner::RootDaemon,
            "the root daemon started",
            "stop the detached root daemon for this cache and retry with the current trufflepig binary",
            &[
                "trufflepig-system.service",
                "trufflepig system stop",
                "workspace coordinator",
            ][..],
        ),
        (
            BackgroundOwner::WorkspaceCoordinator,
            "the workspace coordinator started",
            "stop the detached workspace coordinator for this cache and retry with the current trufflepig binary",
            &[
                "trufflepig-system.service",
                "trufflepig system stop",
                "root daemon",
            ][..],
        ),
        (
            BackgroundOwner::InferenceClient,
            "the inference client started",
            "restart the process through its owning systemd unit, or rerun the caller with the current trufflepig binary",
            &[
                "trufflepig-system.service",
                "trufflepig system stop",
                "root daemon",
                "workspace coordinator",
            ][..],
        ),
    ];

    for (owner, owner_context, recovery, forbidden) in owners {
        let error = validate_background_executable(executable.clone(), &cache, owner).unwrap_err();
        let replaced = error
            .downcast_ref::<ReplacedExecutable>()
            .expect("typed replaced-executable error");
        assert_eq!(replaced.executable, executable);
        assert_eq!(replaced.cache, cache);
        assert_eq!(replaced.owner, owner);
        let message = format!("{error:#}");
        assert!(message.starts_with("router_binary_replaced:"), "{message}");
        assert!(message.contains("/opt/trufflepig (deleted)"), "{message}");
        assert!(message.contains("/cache/root"), "{message}");
        assert!(message.contains(owner_context), "{message}");
        assert!(message.contains(recovery), "{message}");
        for advice in forbidden {
            assert!(
                !message.contains(advice),
                "unexpected {advice:?}: {message}"
            );
        }
    }
}

#[test]
fn live_and_near_suffix_executables_are_accepted() {
    for executable in [
        "/opt/trufflepig",
        "/opt/trufflepig (deleted)-backup",
        "/opt/trufflepig (deleted",
    ] {
        let path = PathBuf::from(executable);
        assert_eq!(
            validate_background_executable(
                path.clone(),
                PathBuf::from("/cache").as_path(),
                BackgroundOwner::SystemRouter,
            )
            .unwrap(),
            path
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn deleted_executable_is_detected_in_a_running_process() {
    const CACHE: &str = "TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_CACHE";
    const ROOT: &str = "TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_ROOT";
    const READY: &str = "TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_READY";
    const RELEASE: &str = "TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_RELEASE";

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("copied-test-binary");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    std::fs::set_permissions(&executable, permissions).unwrap();
    let cache = directory.path().join("root-cache");
    let root = directory.path().join("repository-root");
    std::fs::create_dir(&root).unwrap();
    let ready = directory.path().join("child-ready");
    let release = directory.path().join("child-release");

    let mut command = Command::new(&executable);
    command
        .args([
            "--exact",
            "background_process::tests::deleted_executable_child_entry",
            "--nocapture",
        ])
        .env(CACHE, &cache)
        .env(ROOT, &root)
        .env(READY, &ready)
        .env(RELEASE, &release)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Parallel tests may fork while fs::copy's destination is writable, inheriting its descriptor.
    // That keeps the copied inode busy until the forked process execs and closes the descriptor.
    let spawn_deadline = Instant::now() + Duration::from_secs(1);
    let mut child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                let remaining = spawn_deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "copied test executable remained ETXTBSY for 1 second: {error}"
                );
                std::thread::sleep(Duration::from_millis(10).min(remaining));
                assert!(
                    Instant::now() < spawn_deadline,
                    "copied test executable remained ETXTBSY for 1 second: {error}"
                );
            }
            Err(error) => panic!("failed to spawn copied test process: {error}"),
        }
    };

    let ready_deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < ready_deadline && child.try_wait().unwrap().is_none()
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !ready.exists() {
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        panic!("copied test process did not become ready: {output:?}");
    }

    std::fs::remove_file(&executable).unwrap();
    std::fs::write(&release, b"continue").unwrap();
    let exit_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < exit_deadline && child.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn deleted_executable_child_entry() {
    let (Some(cache), Some(ready), Some(release)) = (
        std::env::var_os("TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_CACHE"),
        std::env::var_os("TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_READY"),
        std::env::var_os("TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_RELEASE"),
    ) else {
        return;
    };
    let ready = PathBuf::from(ready);
    let release = PathBuf::from(release);
    std::fs::write(ready, b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !release.exists() {
        assert!(Instant::now() < deadline, "parent did not release child");
        std::thread::sleep(Duration::from_millis(10));
    }

    let cache = PathBuf::from(cache);
    let raw_executable = std::env::current_exe().unwrap();
    let spawn_error = Command::new(&raw_executable).spawn().unwrap_err();
    assert_eq!(
        spawn_error.kind(),
        io::ErrorKind::NotFound,
        "raw current_exe produces ENOENT after unlink"
    );
    let error = current_background_executable(&cache, BackgroundOwner::SystemRouter).unwrap_err();
    let replaced = error
        .downcast_ref::<ReplacedExecutable>()
        .expect("kernel-deleted executable keeps the typed error");
    assert_eq!(replaced.executable, raw_executable);
    assert_eq!(replaced.cache, cache);
    assert!(background_executable_replaced(&replaced.executable));
    assert!(error.to_string().starts_with("router_binary_replaced:"));
    assert!(error.to_string().contains("root-cache"));
    assert!(error.to_string().contains("trufflepig system stop"));

    let root = std::env::var_os("TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_ROOT")
        .map(PathBuf::from)
        .expect("isolated history root");
    let error = crate::history::worker::start(&root, &cache)
        .expect_err("history start rejects the deleted running executable");
    let replaced = error
        .downcast_ref::<ReplacedExecutable>()
        .expect("history start preserves the typed replaced-executable error");
    assert_eq!(replaced.executable, raw_executable);
    assert_eq!(replaced.cache, cache.canonicalize().unwrap());
    assert_eq!(replaced.owner, BackgroundOwner::RootDaemon);
    let message = format!("{error:#}");
    assert!(message.starts_with("router_binary_replaced:"), "{message}");
    assert!(
        message.contains(cache.canonicalize().unwrap().to_string_lossy().as_ref()),
        "{message}"
    );
    assert!(message.contains("the root daemon started"), "{message}");
    assert!(
        message.contains("stop the detached root daemon for this cache"),
        "{message}"
    );
    assert!(!message.contains("trufflepig system stop"), "{message}");
    assert!(!message.contains("trufflepig-system.service"), "{message}");
}

#[test]
fn spawn_failure_keeps_io_cause_and_names_executable_role_and_cache() {
    let executable = PathBuf::from("/missing/trufflepig-test-executable");
    let cache = PathBuf::from("/cache/test-router");
    let mut command = Command::new(&executable);
    let error = match spawn_background_for(&mut command, &executable, "workspace daemon", &cache) {
        Ok(_) => panic!("missing executable unexpectedly started"),
        Err(error) => error,
    };

    assert!(
        error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<io::Error>())
            .any(|cause| cause.kind() == io::ErrorKind::NotFound),
        "{error:#}"
    );
    let message = format!("{error:#}");
    assert!(message.contains("background_spawn_failed:"), "{message}");
    assert!(message.contains("workspace daemon"), "{message}");
    assert!(
        message.contains("/missing/trufflepig-test-executable"),
        "{message}"
    );
    assert!(message.contains("/cache/test-router"), "{message}");
}

#[test]
fn background_child_owns_process_group_and_is_reaped() {
    let mut command = Command::new("sleep");
    command.arg("60");
    let child = spawn_background(&mut command).expect("spawn background child");
    let child_pid = child.id() as libc::pid_t;
    let child_group = unsafe { libc::getpgid(child_pid) };
    let caller_group = unsafe { libc::getpgrp() };
    assert_eq!(child_group, child_pid);
    assert_ne!(child_group, caller_group);
    assert!(!child.exited());
    assert_eq!(unsafe { libc::kill(child_pid, libc::SIGKILL) }, 0);
    let started = std::time::Instant::now();
    while !child.exited() {
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
