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
    env::split_paths(&env::var_os("PATH")?)
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
