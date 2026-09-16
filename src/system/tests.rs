use super::{dir_from, spool_dir_from};
use std::{ffi::OsString, path::PathBuf};

fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    }
}

#[test]
fn system_dir_override_wins() {
    let dir = dir_from(lookup(&[
        ("TRUFFLEPIG_SYSTEM_DIR", "/override"),
        ("XDG_RUNTIME_DIR", "/run/user"),
        ("XDG_CACHE_HOME", "/xdg-cache"),
        ("HOME", "/home"),
    ]));
    assert_eq!(dir, Some(PathBuf::from("/override")));
}

#[test]
fn runtime_dir_beats_cache_and_home() {
    let dir = dir_from(lookup(&[
        ("XDG_RUNTIME_DIR", "/run/user"),
        ("XDG_CACHE_HOME", "/xdg-cache"),
        ("HOME", "/home"),
    ]));
    assert_eq!(dir, Some(PathBuf::from("/run/user/trufflepig/system")));
}

#[test]
fn cache_home_beats_home() {
    let dir = dir_from(lookup(&[
        ("XDG_CACHE_HOME", "/xdg-cache"),
        ("HOME", "/home"),
    ]));
    assert_eq!(dir, Some(PathBuf::from("/xdg-cache/trufflepig/system")));
}

#[test]
fn home_falls_back_to_dot_cache() {
    let dir = dir_from(lookup(&[("HOME", "/home")]));
    assert_eq!(dir, Some(PathBuf::from("/home/.cache/trufflepig/system")));
}

#[test]
fn no_base_directory_yields_none() {
    assert_eq!(dir_from(lookup(&[])), None);
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
