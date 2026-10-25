use super::{
    BackgroundOwner, ReplacedExecutable, background_executable_replaced,
    current_background_executable, spawn_background, spawn_background_for,
    validate_background_executable,
};
use std::{io, path::PathBuf, process::Command};

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
    crate::test_fixtures::unlinked_executable::run_unlinked(
        "background_process::tests::deleted_executable_child_entry",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn deleted_executable_child_entry() {
    let Some(child) = crate::test_fixtures::unlinked_executable::unlinked_child() else {
        return;
    };
    let cache = child.cache;
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
