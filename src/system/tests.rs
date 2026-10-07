mod board_marker_tests;
mod router_spawn_tests;
mod router_start_tests;
mod router_status_tests;

use super::board_runtime::{BoardDatabaseMarker, board_database_marker_matches};
use super::*;
use std::path::Path;

fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    }
}

fn no_login_session() -> Option<PathBuf> {
    None
}

#[test]
fn system_dir_override_wins() {
    let dir = dir_from(
        lookup(&[
            ("TRUFFLEPIG_SYSTEM_DIR", "/override"),
            ("XDG_RUNTIME_DIR", "/run/user"),
            ("XDG_CACHE_HOME", "/xdg-cache"),
            ("HOME", "/home"),
        ]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/override")));
}

#[test]
fn runtime_dir_beats_cache_and_home() {
    let dir = dir_from(
        lookup(&[
            ("XDG_RUNTIME_DIR", "/run/user"),
            ("XDG_CACHE_HOME", "/xdg-cache"),
            ("HOME", "/home"),
        ]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/run/user/trufflepig/system")));
}

#[test]
fn runtime_dir_beats_login_session() {
    let dir = dir_from(lookup(&[("XDG_RUNTIME_DIR", "/run/user")]), || {
        Some(PathBuf::from("/run/user/1000"))
    });
    assert_eq!(dir, Some(PathBuf::from("/run/user/trufflepig/system")));
}

#[test]
fn login_session_beats_cache_chain() {
    let dir = dir_from(
        lookup(&[("XDG_CACHE_HOME", "/xdg-cache"), ("HOME", "/home")]),
        || Some(PathBuf::from("/run/user/1000")),
    );
    assert_eq!(dir, Some(PathBuf::from("/run/user/1000/trufflepig/system")));
}

#[test]
fn cache_home_beats_home() {
    let dir = dir_from(
        lookup(&[("XDG_CACHE_HOME", "/xdg-cache"), ("HOME", "/home")]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/xdg-cache/trufflepig/system")));
}

#[test]
fn home_falls_back_to_dot_cache() {
    let dir = dir_from(lookup(&[("HOME", "/home")]), no_login_session);
    assert_eq!(dir, Some(PathBuf::from("/home/.cache/trufflepig/system")));
}

#[test]
fn no_base_directory_yields_none() {
    assert_eq!(dir_from(lookup(&[]), no_login_session), None);
}

#[test]
fn empty_override_falls_through_to_the_chain() {
    let dir = dir_from(
        lookup(&[("TRUFFLEPIG_SYSTEM_DIR", ""), ("HOME", "/home")]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/home/.cache/trufflepig/system")));
}

#[test]
fn empty_runtime_dir_falls_through_to_cache_chain() {
    let dir = dir_from(
        lookup(&[
            ("XDG_RUNTIME_DIR", ""),
            ("XDG_CACHE_HOME", "/xdg-cache"),
            ("HOME", "/home"),
        ]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/xdg-cache/trufflepig/system")));
}

#[test]
fn relative_values_fall_through_to_home() {
    let dir = dir_from(
        lookup(&[
            ("TRUFFLEPIG_SYSTEM_DIR", "relative/override"),
            ("XDG_RUNTIME_DIR", "relative/runtime"),
            ("XDG_CACHE_HOME", "relative/cache"),
            ("HOME", "/home"),
        ]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/home/.cache/trufflepig/system")));
}

#[test]
fn spool_dir_override_wins() {
    let dir = spool_dir_from(lookup(&[("TRUFFLEPIG_SPOOL_DIR", "/override")]), 1000);
    assert_eq!(dir, PathBuf::from("/override"));
}

#[test]
fn spool_dir_defaults_to_per_user_tmp() {
    let dir = spool_dir_from(lookup(&[("XDG_RUNTIME_DIR", "/run/user")]), 1000);
    assert_eq!(dir, PathBuf::from("/tmp/trufflepig-1000/spool"));
}
