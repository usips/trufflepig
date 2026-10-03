use super::*;

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
            BoardOp::FeedbackList { open_only: false },
        ))
        .unwrap();
    let BoardResult::Feedback(entries) = reply.result else {
        panic!("expected feedback list");
    };
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
                BoardOp::Show { target: None }
            ))
            .is_err()
    );
}
