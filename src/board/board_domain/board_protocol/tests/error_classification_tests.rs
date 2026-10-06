use super::*;

#[test]
fn leading_error_codes_ignore_untrusted_diagnostic_segments() {
    for message in [
        "daemon: daemon: invalid_body: text mentions : invalid_actor: nope",
        "invalid_body: text mentions : database is locked: nope",
    ] {
        let error = anyhow::anyhow!(message).context("route request");
        assert_eq!(
            BoardErrorCode::from_error(&error),
            Some(BoardErrorCode::InvalidBody)
        );
    }
    for message in [
        "failed transport: text mentions : invalid_body: nope",
        "board_unavailable_note: invalid_actor: nope",
        "daemon: failure mentions database is locked",
    ] {
        assert_eq!(BoardErrorCode::from_error(&anyhow::anyhow!(message)), None);
    }
    let typed = BoardError::new(BoardErrorCode::ClaimConflict, "invalid_body: quoted text");
    let error = anyhow::Error::new(typed).context("dispatch board");
    assert_eq!(
        BoardErrorCode::from_error(&error),
        Some(BoardErrorCode::ClaimConflict)
    );
    let sqlite = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
        Some("unexpected contention wording".into()),
    );
    assert_eq!(
        BoardErrorCode::from_error(&anyhow::Error::new(sqlite).context("store entry")),
        Some(BoardErrorCode::DatabaseLocked),
    );
}

#[test]
fn type_mismatch_is_invalid_state() {
    let sqlite = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
        Some("datatype mismatch".into()),
    );
    let error = anyhow::Error::new(sqlite).context("store entry");
    assert_eq!(
        BoardErrorCode::from_error(&error),
        Some(BoardErrorCode::InvalidState)
    );
    assert_eq!(BoardError::from(error).code, BoardErrorCode::InvalidState);
}

#[test]
fn sqlite_storage_failures_classify_by_typed_code() {
    for (raw, expected) in [
        (rusqlite::ffi::SQLITE_BUSY, BoardErrorCode::DatabaseLocked),
        (rusqlite::ffi::SQLITE_LOCKED, BoardErrorCode::DatabaseLocked),
        (
            rusqlite::ffi::SQLITE_CONSTRAINT,
            BoardErrorCode::InvalidState,
        ),
        (
            rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
            BoardErrorCode::InvalidState,
        ),
        (
            rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
            BoardErrorCode::InvalidState,
        ),
        (
            rusqlite::ffi::SQLITE_CORRUPT,
            BoardErrorCode::BoardUnavailable,
        ),
        (
            rusqlite::ffi::SQLITE_NOTADB,
            BoardErrorCode::BoardUnavailable,
        ),
        (rusqlite::ffi::SQLITE_TOOBIG, BoardErrorCode::InvalidBody),
        (rusqlite::ffi::SQLITE_MISMATCH, BoardErrorCode::InvalidState),
        (
            rusqlite::ffi::SQLITE_IOERR,
            BoardErrorCode::BoardUnavailable,
        ),
    ] {
        let sqlite = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(raw),
            Some("simulated storage failure".into()),
        );
        let error = anyhow::Error::new(sqlite).context("store entry");
        assert_eq!(BoardErrorCode::from_error(&error), Some(expected), "{raw}");
        assert_eq!(BoardError::from(error).code, expected, "{raw}");
    }
}

#[test]
fn typed_errors_win_over_conflicting_textual_contexts() {
    let error = anyhow::Error::new(BoardError::new(BoardErrorCode::ClaimConflict, "claimed"))
        .context("database is locked: while dispatching");
    assert_eq!(
        BoardErrorCode::from_error(&error),
        Some(BoardErrorCode::ClaimConflict)
    );
    assert_eq!(BoardError::from(error).code, BoardErrorCode::ClaimConflict);
    let sqlite = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some("database is locked: quoted diagnostic".into()),
    );
    assert_eq!(
        BoardErrorCode::from_error(&anyhow::Error::new(sqlite)),
        Some(BoardErrorCode::InvalidState)
    );
    for message in [
        "database is locked",
        "daemon: database is locked",
        "daemon: daemon: database is locked",
    ] {
        assert_eq!(
            BoardErrorCode::from_error(&anyhow::anyhow!(message)),
            Some(BoardErrorCode::DatabaseLocked)
        );
    }
}

#[test]
fn typed_errors_preserve_domain_prefixes_and_database_lock_classification() {
    let stale = BoardError::from(anyhow::anyhow!("stale_revision: expected P1@2"));
    assert_eq!(stale.code, BoardErrorCode::StaleRevision);
    assert_eq!(stale.to_string(), "stale_revision: expected P1@2");
    let contextual =
        BoardError::from(anyhow::anyhow!("invalid_actor: not steward").context("accept proposal"));
    assert_eq!(contextual.code, BoardErrorCode::InvalidActor);
    let locked = BoardError::from(anyhow::anyhow!("database is locked"));
    assert_eq!(locked.code, BoardErrorCode::DatabaseLocked);
    let unavailable = BoardError::from(anyhow::anyhow!("unable to open database file"));
    assert_eq!(unavailable.code, BoardErrorCode::BoardUnavailable);
}
