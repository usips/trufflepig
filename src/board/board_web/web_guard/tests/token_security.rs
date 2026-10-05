use super::*;
use std::{
    fs::{self, File},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

#[test]
fn read_only_token_discovery_never_creates_or_repairs_a_file() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let path = directory.path().join("board-web.token");
    assert!(BoardWebToken::read_at(&path).is_err());
    assert!(!path.exists());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    let token = BoardWebToken::rotate_at(&path).unwrap();
    assert_eq!(
        BoardWebToken::read_at(&path).unwrap().expose(),
        token.expose()
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(BoardWebToken::read_at(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), token.expose().as_bytes());
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o644);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn concurrent_rotations_each_publish_a_complete_token() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let path = directory.path().join("board-web.token");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                BoardWebToken::rotate_at(&path).unwrap().expose().to_owned()
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(results.iter().all(|token| token.len() == 64));
    assert!(results.contains(&fs::read_to_string(&path).unwrap()));
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn incomplete_abandoned_candidate_does_not_poison_final_token() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let abandoned = directory.path().join(".board-web-token-abandoned.pending");
    fs::write(&abandoned, b"partial").unwrap();
    fs::set_permissions(&abandoned, fs::Permissions::from_mode(0o600)).unwrap();
    let path = directory.path().join("board-web.token");
    let token = BoardWebToken::rotate_at(&path).unwrap();
    assert_eq!(
        BoardWebToken::read_at(&path).unwrap().expose(),
        token.expose()
    );
    assert_eq!(fs::read(&abandoned).unwrap(), b"partial");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
}

#[test]
fn token_is_private_random_and_stable_across_reads() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let path = directory.path().join("board-web.token");
    let token = BoardWebToken::rotate_at(&path).unwrap();
    let first = BoardWebToken::read_at(&path).unwrap();
    let second = BoardWebToken::read_at(&path).unwrap();
    let other = BoardWebToken::rotate_at(&directory.path().join("other.token")).unwrap();
    assert_eq!(first.expose(), token.expose());
    assert_eq!(first.expose(), second.expose());
    assert_ne!(first.expose(), other.expose());
    assert_eq!(first.expose().len(), 64);
    assert!(first.expose().bytes().all(|byte| byte.is_ascii_hexdigit()));
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    // SAFETY: geteuid has no preconditions and cannot fail.
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
}

#[test]
fn token_comparison_checks_every_position_and_length() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    assert!(token.matches(token.expose()));
    for index in 0..64 {
        let mut candidate = token.expose().as_bytes().to_vec();
        candidate[index] = if candidate[index] == b'a' { b'b' } else { b'a' };
        assert!(!token.matches(std::str::from_utf8(&candidate).unwrap()));
    }
    assert!(!token.matches(""));
    assert!(!token.matches(&token.expose()[..63]));
    assert!(!token.matches(&format!("{}a", token.expose())));
}

#[test]
fn reads_reject_symlink_and_never_change_its_target() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let target = directory.path().join("target");
    let token = BoardWebToken::rotate_at(&target).unwrap();
    let link = directory.path().join("symlink.token");
    symlink(&target, &link).unwrap();
    assert!(BoardWebToken::read_at(&link).is_err());
    assert_eq!(fs::read(&target).unwrap(), token.expose().as_bytes());
}

#[test]
fn reads_reject_unsafe_modes_and_preserve_existing_bytes() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let path = directory.path().join("token");
    let token = BoardWebToken::rotate_at(&path).unwrap();
    for mode in [0o644, 0o660, 0o700, 0o4600] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert!(BoardWebToken::read_at(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), token.expose().as_bytes());
    }
}

#[test]
fn reads_reject_malformed_regular_files_and_non_regular_files() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let path = directory.path().join("token");
    File::create(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    for bytes in [
        vec![],
        vec![b'a'; 63],
        vec![b'a'; 65],
        vec![b'z'; 64],
        vec![0; 64],
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(BoardWebToken::read_at(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    assert!(BoardWebToken::read_at(directory.path()).is_err());
    let fifo = directory.path().join("fifo");
    let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: fifo_name is a valid nul-terminated path and mode is a permission mask.
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    assert!(BoardWebToken::read_at(&fifo).is_err());
}

#[test]
fn every_rotation_publishes_a_fresh_0600_token() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let path = directory.path().join("board-web.token");
    let first = BoardWebToken::rotate_at(&path).unwrap();
    let second = BoardWebToken::rotate_at(&path).unwrap();
    assert_ne!(first.expose(), second.expose());
    assert_eq!(
        BoardWebToken::read_at(&path).unwrap().expose(),
        second.expose()
    );
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    // SAFETY: geteuid has no preconditions and cannot fail.
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn rotation_replaces_planted_links_without_writing_through() {
    let directory = crate::board::board_test_support::scratch("web-token-");
    let target = directory.path().join("elsewhere");
    fs::write(&target, b"untouched").unwrap();
    let path = directory.path().join("board-web.token");
    symlink(&target, &path).unwrap();
    let token = BoardWebToken::rotate_at(&path).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"untouched");
    assert!(
        !fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        BoardWebToken::read_at(&path).unwrap().expose(),
        token.expose()
    );
}
