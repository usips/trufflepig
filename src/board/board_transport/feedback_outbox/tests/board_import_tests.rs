use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn database_failure_retains_report_and_post_commit_replay_removes_it_once() {
    let dir = scratch();
    let request = report();
    queue(dir.path(), &request).unwrap();
    let mut backend = ImportBackend {
        unavailable: true,
        ..ImportBackend::default()
    };
    assert_eq!(import_pending(dir.path(), &mut backend).unwrap().pending, 1);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    backend.unavailable = false;
    // Simulate commit followed by process death before unlinking the record.
    backend.handle(&request).unwrap();
    assert_eq!(
        import_pending(dir.path(), &mut backend).unwrap().imported,
        1
    );
    assert_eq!(backend.entries.len(), 1);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    assert_eq!(
        import_pending(dir.path(), &mut backend).unwrap(),
        ImportSummary::default()
    );
}

#[test]
fn malformed_or_mismatched_records_are_quarantined_without_execution() {
    let dir = scratch();
    fs::write(
        dir.path().join(format!("{}.feedback", new_import_key())),
        b"not JSON",
    )
    .unwrap();
    let mut request = report();
    queue(dir.path(), &request).unwrap();
    let BoardOp::Feedback { import_key, .. } = &mut request.op else {
        unreachable!()
    };
    let key = import_key.clone().unwrap();
    *import_key = Some(new_import_key());
    let bytes = serde_json::to_vec(&QueuedFeedback {
        import_key: key.clone(),
        request,
    })
    .unwrap();
    fs::write(dir.path().join(format!("{key}.feedback")), bytes).unwrap();
    let mut backend = ImportBackend::default();
    let summary = import_pending(dir.path(), &mut backend).unwrap();
    assert_eq!(summary.quarantined, 2);
    assert!(backend.entries.is_empty());
    for entry in fs::read_dir(dir.path()).unwrap() {
        assert_eq!(entry.unwrap().path().extension().unwrap(), "quarantine");
    }
}

#[test]
fn semantic_import_rejections_quarantine_and_transient_rejections_remain_pending() {
    for code in [
        BoardErrorCode::InvalidReference,
        BoardErrorCode::BoardApiMismatch,
        BoardErrorCode::InvalidState,
        BoardErrorCode::InvalidBody,
        BoardErrorCode::InvalidKind,
        BoardErrorCode::BoardUnavailable,
        BoardErrorCode::DatabaseLocked,
    ] {
        let directory = scratch();
        queue(directory.path(), &report()).unwrap();
        let summary = import_pending(directory.path(), &mut RejectedImport(code)).unwrap();
        let transient = matches!(
            code,
            BoardErrorCode::BoardUnavailable | BoardErrorCode::DatabaseLocked
        );
        assert_eq!(summary.pending, usize::from(transient), "{code:?}");
        assert_eq!(summary.quarantined, usize::from(!transient), "{code:?}");
        let path = fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            path.extension().unwrap(),
            if transient { "feedback" } else { "quarantine" }
        );
    }
}

#[test]
fn deterministic_sqlite_errors_quarantine_once_while_locks_stay_pending() {
    for (raw_code, quarantines) in [
        (rusqlite::ffi::SQLITE_CONSTRAINT, true),
        (rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE, true),
        (rusqlite::ffi::SQLITE_TOOBIG, true),
        (rusqlite::ffi::SQLITE_MISMATCH, true),
        (rusqlite::ffi::SQLITE_CORRUPT, false),
        (rusqlite::ffi::SQLITE_NOTADB, false),
        (rusqlite::ffi::SQLITE_BUSY, false),
        (rusqlite::ffi::SQLITE_LOCKED, false),
    ] {
        let directory = scratch();
        queue(directory.path(), &report()).unwrap();
        let mut backend = SqliteRejectedImport {
            raw_code,
            attempts: 0,
        };
        let first = import_pending(directory.path(), &mut backend).unwrap();
        assert_eq!(backend.attempts, 1, "{raw_code}");
        assert_eq!(first.quarantined, usize::from(quarantines), "{raw_code}");
        assert_eq!(first.pending, usize::from(!quarantines), "{raw_code}");
        let second = import_pending(directory.path(), &mut backend).unwrap();
        let path = fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        if quarantines {
            assert_eq!(second, ImportSummary::default(), "{raw_code}");
            assert_eq!(backend.attempts, 1, "{raw_code}");
            assert_eq!(path.extension().unwrap(), "quarantine", "{raw_code}");
        } else {
            assert_eq!(second.pending, 1, "{raw_code}");
            assert_eq!(backend.attempts, 2, "{raw_code}");
            assert_eq!(path.extension().unwrap(), "feedback", "{raw_code}");
        }
    }
}

#[test]
fn garbage_database_imports_stay_pending_without_quarantine() {
    let directory = scratch();
    let db_path = directory.path().join("board.sqlite3");
    fs::write(&db_path, b"this is not a sqlite database").unwrap();
    let spool = directory.path().join("spool");
    queue(&spool, &report()).unwrap();
    let mut backend = CorruptDbImport {
        conn: rusqlite::Connection::open(&db_path).unwrap(),
    };
    let summary = import_pending(&spool, &mut backend).unwrap();
    assert_eq!(summary.pending, 1);
    assert_eq!(summary.quarantined, 0);
    assert_eq!(summary.imported, 0);
    let path = fs::read_dir(&spool)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(path.extension().unwrap(), "feedback");
    // A second tick still retries instead of quarantining.
    let retry = import_pending(&spool, &mut backend).unwrap();
    assert_eq!(retry.pending, 1);
    assert_eq!(retry.quarantined, 0);
}

#[test]
fn imported_feedback_sets_server_provenance_and_direct_feedback_does_not() {
    let directory = scratch();
    let config = crate::board::BoardConfig::for_database(directory.path().join("board.sqlite3"));
    let mut backend = crate::board::local_board::LocalBoard::open(&config).unwrap();
    let direct = report();
    backend.handle(&direct).unwrap();
    let mut imported = report();
    let BoardOp::Feedback { summary, .. } = &mut imported.op else {
        unreachable!();
    };
    *summary = EntryText::new("Imported through trusted outbox dispatch").unwrap();
    let spool = directory.path().join("spool");
    queue(&spool, &imported).unwrap();
    assert_eq!(import_pending(&spool, &mut backend).unwrap().imported, 1);
    let durable = rusqlite::Connection::open(&config.db_path).unwrap();
    let via = durable
        .prepare("SELECT via FROM entries ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(via, [None, Some("outbox".into())]);
    let reply = backend
        .handle(&BoardRequest::new(
            imported.actor.clone(),
            BoardOp::FeedbackList {
                open_only: false,
                after: None,
                through: None,
                limit: 200,
            },
        ))
        .unwrap();
    let BoardResult::Feedback(page) = reply.result else {
        panic!("expected feedback list");
    };
    let entries = page.feedback;
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries
            .iter()
            .filter(|feedback| feedback.entry.via
                == Some(crate::board::board_protocol::FeedbackVia::Outbox))
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|feedback| feedback.entry.via.is_none())
            .count(),
        1
    );
    assert!(
        backend
            .import_feedback(&BoardRequest::new(
                direct.actor,
                BoardOp::Overview {
                    scope: ReadScope::All,
                    after: None,
                    through: None,
                    limit: 200,
                }
            ))
            .is_err()
    );
}

#[test]
fn oversized_record_quarantines_on_first_attempt_with_size_code() {
    let sqlite = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_TOOBIG),
        Some("String or BLOB exceeds size limit".into()),
    );
    assert_eq!(
        BoardError::from(anyhow::Error::new(sqlite).context("import feedback")).code,
        BoardErrorCode::InvalidBody,
    );
    let directory = scratch();
    queue(directory.path(), &report()).unwrap();
    let mut backend = SqliteRejectedImport {
        raw_code: rusqlite::ffi::SQLITE_TOOBIG,
        attempts: 0,
    };
    let first = import_pending(directory.path(), &mut backend).unwrap();
    assert_eq!(backend.attempts, 1);
    assert_eq!(first.quarantined, 1);
    assert_eq!(first.pending, 0);
    let second = import_pending(directory.path(), &mut backend).unwrap();
    assert_eq!(second, ImportSummary::default());
    assert_eq!(backend.attempts, 1);
}
