use super::*;

const ROUTER: &str =
    "/user.slice/user-1000.slice/user@1000.service/app.slice/trufflepig-system.service";

#[test]
fn placement_needs_a_valid_slice_and_a_service_cgroup() {
    let placement =
        DaemonPlacement::new(Some("app-trufflepig.slice".into()), Some(ROUTER.into())).unwrap();
    let mut command = Command::new("true");
    placement.apply(&mut command);
    let envs: Vec<_> = command.get_envs().collect();
    assert!(envs.contains(&(
        std::ffi::OsStr::new(DAEMON_SLICE_ENV),
        Some(std::ffi::OsStr::new("app-trufflepig.slice"))
    )));
    assert!(envs.contains(&(
        std::ffi::OsStr::new(SCOPE_FROM_ENV),
        Some(std::ffi::OsStr::new(ROUTER))
    )));

    for (slice, cgroup) in [
        (None, Some(ROUTER)),
        (Some("app-trufflepig.slice"), None),
        (Some("app trufflepig.slice"), Some(ROUTER)),
        (Some("trufflepig.service"), Some(ROUTER)),
        (Some(".slice"), Some(ROUTER)),
        (
            Some("app-trufflepig.slice"),
            Some("/user.slice/session-2.scope"),
        ),
    ] {
        assert!(
            DaemonPlacement::new(slice.map(Into::into), cgroup.map(Into::into)).is_none(),
            "{slice:?} {cgroup:?}"
        );
    }
}

#[test]
fn only_a_child_still_in_the_router_cgroup_requests_a_scope() {
    let request = ScopeRequest::new(4242, "app-trufflepig.slice", ROUTER, ROUTER).unwrap();
    assert_eq!(request.unit, "trufflepig-system.service");
    let in_scope = "/user.slice/user-1000.slice/user@1000.service/app.slice/app-trufflepig.slice/trufflepig-daemon-4000.scope";
    assert!(
        ScopeRequest::new(4243, "app-trufflepig.slice", ROUTER, in_scope).is_none(),
        "a grandchild inherits its parent's scope"
    );
}

#[test]
fn scope_request_binds_the_daemon_to_the_router_unit_in_the_slice() {
    let request = ScopeRequest::new(4242, "app-trufflepig.slice", ROUTER, ROUTER).unwrap();
    let args = request.busctl_args();
    assert_eq!(
        args,
        [
            "--user",
            "--timeout=1",
            "call",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "StartTransientUnit",
            "ssa(sv)a(sa(sv))",
            "trufflepig-daemon-4242.scope",
            "fail",
            "5",
            "Description",
            "s",
            "trufflepig background daemon",
            "PIDs",
            "au",
            "1",
            "4242",
            "Slice",
            "s",
            "app-trufflepig.slice",
            "BindsTo",
            "as",
            "1",
            "trufflepig-system.service",
            "CollectMode",
            "s",
            "inactive-or-failed",
            "0",
        ]
    );
}

#[test]
fn cgroup_path_reads_the_unified_hierarchy_entry() {
    let text = format!("1:net_cls:/\n0::{ROUTER}\n");
    assert_eq!(cgroup_path(&text).as_deref(), Some(ROUTER));
    assert_eq!(cgroup_path("1:net_cls:/\n"), None);
    assert!(own_cgroup().is_none_or(|path| path.starts_with('/')));
}

#[test]
fn a_process_without_placement_environment_stays_put() {
    // The test process has no placement variables; this must not spawn busctl.
    if std::env::var_os(SCOPE_FROM_ENV).is_none() {
        let before = own_cgroup();
        enter_from_environment();
        assert_eq!(own_cgroup(), before);
    }
}
