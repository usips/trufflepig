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

#[test]
fn unchanged_private_database_marker_preserves_inode_and_mtime() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::time::{Duration, SystemTime};
    let directory = crate::board::board_test_support::scratch("board-marker-idempotence-");
    let first_database = directory.path().join("first.sqlite3");
    super::record_board_database(directory.path(), &first_database).unwrap();
    let marker = directory.path().join("board-backend.json");
    let old_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    std::fs::File::open(&marker)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old_time))
        .unwrap();
    let before = marker.metadata().unwrap();
    super::record_board_database(directory.path(), &first_database).unwrap();
    let unchanged = marker.metadata().unwrap();
    assert_eq!(unchanged.ino(), before.ino());
    assert_eq!(unchanged.modified().unwrap(), before.modified().unwrap());
    assert_eq!(unchanged.permissions().mode() & 0o7777, 0o600);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

    let second_database = directory.path().join("second.sqlite3");
    super::record_board_database(directory.path(), &second_database).unwrap();
    let updated = marker.metadata().unwrap();
    assert_ne!(updated.ino(), before.ino());
    let marker: super::BoardDatabaseMarker =
        serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    assert_eq!(marker.database, second_database);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn database_marker_rejects_symlinks_nonprivate_files_and_other_owners() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let directory = crate::board::board_test_support::scratch("board-marker-security-");
    let database = directory.path().join("board.sqlite3");
    let marker = directory.path().join("board-backend.json");
    let outside = directory.path().join("outside");
    std::fs::write(&outside, b"private outside data").unwrap();
    std::os::unix::fs::symlink(&outside, &marker).unwrap();
    assert!(super::record_board_database(directory.path(), &database).is_err());
    assert!(marker.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(&outside).unwrap(), b"private outside data");
    std::fs::remove_file(&marker).unwrap();

    super::record_board_database(directory.path(), &database).unwrap();
    let bytes = std::fs::read(&marker).unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(super::record_board_database(directory.path(), &database).is_err());
    assert_eq!(std::fs::read(&marker).unwrap(), bytes);
    assert_eq!(
        marker.metadata().unwrap().permissions().mode() & 0o7777,
        0o644
    );
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    let actual_owner = marker.metadata().unwrap().uid();
    assert!(
        super::board_database_marker_matches(&marker, &bytes, actual_owner.wrapping_add(1))
            .is_err()
    );
    assert_eq!(std::fs::read(&marker).unwrap(), bytes);

    std::fs::remove_file(&marker).unwrap();
    std::fs::create_dir(&marker).unwrap();
    assert!(super::record_board_database(directory.path(), &database).is_err());
    assert!(marker.is_dir());
}
