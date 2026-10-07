//! Verify the build script's tool cfgs against the tools found on `PATH`.
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

/// True when `path` is a regular file with an execute bit set.
/// Non-Unix targets accept any regular file (no exec-bit model).
fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Resolve `name` to an executable file via a `PATH` search.
fn resolve_on_path(name: &str) -> Option<PathBuf> {
    resolve_on_path_value(&env::var_os("PATH")?, name)
}

/// Resolve `name` against a `PATH` value, ignoring empty entries like the build script.
fn resolve_on_path_value(path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// Run `path --version`, returning stdout on success.
fn version_output(path: &Path) -> Option<Vec<u8>> {
    Command::new(path)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
}

/// Parse `git --version` stdout into `(major, minor)`.
fn parse_git_version(bytes: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut parts = text.split_whitespace().nth(2)?.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// Parse `node --version` stdout into its major version.
fn parse_node_version(bytes: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.trim()
        .strip_prefix('v')?
        .split('.')
        .next()?
        .parse()
        .ok()
}

#[test]
fn build_cfg_matches_tools_on_path() {
    let expected = resolve_on_path("node")
        .and_then(|node| version_output(&node))
        .and_then(|stdout| parse_node_version(&stdout))
        .is_some_and(|major| major >= 20);
    assert_eq!(
        cfg!(board_node_20),
        expected,
        "board_node_20 disagrees with the node on PATH (PATH changed since the build?)"
    );
}

#[test]
fn build_cfg_matches_git_on_path() {
    let version = resolve_on_path("git")
        .and_then(|git| version_output(&git))
        .and_then(|stdout| parse_git_version(&stdout));
    assert_eq!(
        cfg!(board_git_2_55),
        version.is_some_and(|v| v >= (2, 55)),
        "board_git_2_55 disagrees with the git on PATH (PATH changed since the build?)"
    );
    assert_eq!(
        cfg!(board_git_2_46),
        version.is_some_and(|v| v >= (2, 46)),
        "board_git_2_46 disagrees with the git on PATH (PATH changed since the build?)"
    );
}

#[cfg(unix)]
#[test]
fn empty_path_entries_do_not_select_tools_from_child_cwd() {
    const CHILD_MARKER: &str = "TRUFFLEPIG_BUILD_TOOL_PROBE_CHILD";
    const REAL_PATH: &str = "TRUFFLEPIG_BUILD_TOOL_PROBE_REAL_PATH";

    if env::var_os(CHILD_MARKER).is_some() {
        let real_path = env::var_os(REAL_PATH).expect("child receives the inherited PATH");
        let cwd = env::current_dir().expect("child has a working directory");

        for tool in ["node", "git"] {
            let cwd_tool = cwd.join(tool);
            assert!(
                is_executable_file(&cwd_tool),
                "fixture {tool} must be executable"
            );
            let fake_version = version_output(&cwd_tool).expect("fake tool prints its old version");
            if tool == "node" {
                assert_eq!(parse_node_version(&fake_version), Some(0));
            } else {
                assert_eq!(parse_git_version(&fake_version), Some((0, 0)));
            }
            assert_eq!(
                resolve_on_path(tool),
                resolve_on_path_value(&real_path, tool),
                "empty PATH entries must not select {tool} from the current directory"
            );
            assert_ne!(resolve_on_path(tool).as_deref(), Some(cwd_tool.as_path()));
        }

        // Exercise the actual parity checks with this isolated child PATH.
        build_cfg_matches_tools_on_path();
        build_cfg_matches_git_on_path();
        return;
    }

    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    let tmpdir = env::var_os("TMPDIR").expect("TMPDIR points at the configured scratch area");
    let fixture = tempfile::Builder::new()
        .prefix("trufflepig-empty-path-probe-")
        .tempdir_in(tmpdir)
        .expect("create isolated PATH fixture");
    for (tool, body) in [
        ("node", "#!/bin/sh\nprintf 'v0.0.0\\n'\n"),
        ("git", "#!/bin/sh\nprintf 'git version 0.0.0\\n'\n"),
    ] {
        let path = fixture.path().join(tool);
        fs::write(&path, body).expect("write fake current-directory tool");
        let mut permissions = fs::metadata(&path)
            .expect("read fake tool metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("make fake tool executable");
    }

    let inherited_path = env::var_os("PATH").expect("test process has PATH");
    let parent_cwd = env::current_dir().expect("test process has a working directory");
    let real_path = env::join_paths(
        env::split_paths(&inherited_path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| {
                if dir.is_absolute() {
                    dir
                } else {
                    parent_cwd.join(dir)
                }
            }),
    )
    .expect("preserve inherited PATH without empty or cwd-relative entries");
    let child_path =
        env::join_paths(std::iter::once(PathBuf::new()).chain(env::split_paths(&real_path)))
            .expect("prefix inherited PATH with an empty entry");
    let child = Command::new(env::current_exe().expect("test executable path"))
        .arg("--exact")
        .arg("empty_path_entries_do_not_select_tools_from_child_cwd")
        .arg("--nocapture")
        .current_dir(fixture.path())
        .env("PATH", child_path)
        .env(CHILD_MARKER, "1")
        .env(REAL_PATH, real_path)
        .output()
        .expect("run PATH regression in an isolated child process");
    assert!(
        child.status.success(),
        "isolated PATH regression failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}
