use super::*;
use std::{io, time::Duration};

#[test]
fn classifies_only_selected_cache_refusals() {
    let cache = Path::new("/chosen/cache");
    for code in [
        libc::EACCES,
        libc::EPERM,
        libc::EROFS,
        libc::ENOTDIR,
        libc::EEXIST,
    ] {
        let error = index_cache_unavailable(cache, io::Error::from_raw_os_error(code).into());
        assert!(
            is_index_cache_unavailable(&error),
            "errno {code}: {error:#}"
        );
    }

    for code in [libc::EMFILE, libc::ENFILE, libc::EIO] {
        let error = index_cache_unavailable(cache, io::Error::from_raw_os_error(code).into());
        assert!(
            !is_index_cache_unavailable(&error),
            "errno {code}: {error:#}"
        );
    }

    for sqlite_code in [
        rusqlite::ffi::SQLITE_CORRUPT,
        rusqlite::ffi::SQLITE_NOTADB,
        rusqlite::ffi::SQLITE_BUSY,
    ] {
        let error = index_cache_unavailable(
            cache,
            SqliteError::SqliteFailure(rusqlite::ffi::Error::new(sqlite_code), None).into(),
        );
        assert!(
            !is_index_cache_unavailable(&error),
            "SQLite code {sqlite_code}: {error:#}"
        );
    }

    let read_only = index_cache_unavailable(
        cache,
        SqliteError::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_READONLY),
            None,
        )
        .into(),
    );
    assert!(is_index_cache_unavailable(&read_only), "{read_only:#}");
}

#[test]
fn cannot_open_selected_cache_carries_recovery_hint() {
    let cache = Path::new("/chosen/cache");
    let error = index_cache_unavailable(
        cache,
        SqliteError::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            None,
        )
        .into(),
    );
    assert!(is_index_cache_unavailable(&error), "{error:#}");
    let text = format!("{error:#}");
    assert!(text.contains("/chosen/cache"), "{text}");
    assert!(text.contains("--cache"), "{text}");
    assert!(text.contains("XDG_CACHE_HOME"), "{text}");
}

#[test]
fn bare_cannot_open_without_selected_cache_stays_untyped() {
    let error: anyhow::Error = SqliteError::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
        None,
    )
    .into();
    assert!(!is_index_cache_unavailable(&error), "{error:#}");
}

#[test]
fn bare_permission_and_path_errors_need_the_selected_cache_marker() {
    for error in [
        io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
        io::Error::from_raw_os_error(libc::ENOTDIR),
    ] {
        assert!(
            !is_index_cache_unavailable(&error.into()),
            "bare filesystem errors stay untyped"
        );
    }
}

#[test]
fn deadline_contexts_take_precedence() {
    let cache = Path::new("/chosen/cache");
    let cause = io::Error::from_raw_os_error(libc::EACCES);
    let marked = index_cache_unavailable(cache, cause.into());
    assert!(is_index_cache_unavailable(&marked), "{marked:#}");
    let wrapped = marked.with_coded_context(|| coded!(TimedOut, "query ended"));
    assert!(!is_index_cache_unavailable(&wrapped), "{wrapped:#}");
    assert!(ErrorCode::TimedOut.is_code_of(&wrapped), "{wrapped:#}");
    let rewrapped = index_cache_unavailable(cache, wrapped);
    assert_eq!(error_code_count(&rewrapped), 2, "{rewrapped:#}");
}

fn error_code_count(error: &anyhow::Error) -> usize {
    error
        .chain()
        .filter(|cause| cause.is::<crate::coded_error::CodedError>())
        .count()
}

#[test]
fn refusal_context_names_the_selected_cache_and_recovery_options() {
    let cache = Path::new("/chosen/cache");
    let error = index_cache_unavailable(cache, io::Error::from_raw_os_error(libc::EACCES).into());
    let text = format!("{error:#}");
    assert!(text.contains("/chosen/cache"), "{text}");
    assert!(text.contains("--cache"), "{text}");
    assert!(text.contains("XDG_CACHE_HOME"), "{text}");
}

#[test]
fn existing_results_directory_is_an_explicit_invalid_cache_shape() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let cache = scratch.path().join("cache");
    fs::create_dir_all(cache.join("results.sqlite3"))?;

    let error = preflight_results_cache(&cache, None).expect_err("database directory is invalid");
    assert!(is_index_cache_unavailable(&error), "{error:#}");
    let text = format!("{error:#}");
    assert!(text.contains("results.sqlite3"), "{text}");
    assert!(text.contains("--cache"), "{text}");
    assert!(text.contains("XDG_CACHE_HOME"), "{text}");
    Ok(())
}

#[test]
fn preflight_probes_a_real_read_only_selected_cache() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir()?;
    let cache = scratch.path().join("selected-cache");
    fs::create_dir(&cache)?;
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o555))?;
    let result = preflight_results_cache(&cache, None);
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o755))?;

    let error = result.expect_err("a read-only selected cache must fail before query work");
    assert!(is_index_cache_unavailable(&error), "{error:#}");
    let contextual = index_cache_unavailable(&cache, error);
    let text = format!("{contextual:#}");
    assert!(text.contains(&cache.display().to_string()), "{text}");
    assert!(text.contains("--cache"), "{text}");
    assert!(text.contains("XDG_CACHE_HOME"), "{text}");
    Ok(())
}

#[test]
fn expired_preflight_does_not_create_the_selected_cache() {
    let scratch = tempfile::tempdir().unwrap();
    let cache = scratch.path().join("not-created");
    let deadline = QueryDeadline::after(Duration::ZERO);

    let error = preflight_results_cache(&cache, Some(&deadline)).expect_err("deadline expired");
    assert!(ErrorCode::TimedOut.is_code_of(&error), "{error:#}");
    assert!(!cache.exists());
}

#[test]
fn successful_preflight_removes_its_probe() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let cache = scratch.path().join("selected-cache");

    preflight_results_cache(&cache, None)?;
    assert!(cache.is_dir());
    assert!(fs::read_dir(&cache)?.next().is_none());
    Ok(())
}
