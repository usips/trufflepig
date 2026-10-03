use super::super::*;
use super::{RejectedImport, scratch};
use crate::board::board_protocol::{BOARD_API, FeedbackVia};
use crate::board::local_board::LocalBoard;

// Persisted API 1 feedback layout, independent of the current serializer.
const SAVED_M1_FEEDBACK: &str = r#"{
  "import_key": "a1533bc0-7ff5-4ea8-8a76-1565ea6af29e",
  "request": {
    "api": 1,
    "actor": {
      "user": "josh",
      "host": "laptop",
      "harness": "codex",
      "session": "m1-spool-session"
    },
    "op": {
      "op": "feedback",
      "kind": "blocked",
      "summary": "Router unavailable during source search",
      "body": "Tried search sym:BoardHost.\nRead the source file to continue.",
      "plan": null,
      "metadata": {
        "version": "0.1.0",
        "build_id": null,
        "repo_key": null,
        "cwd": "src",
        "steer_mode": null,
        "recent_calls": []
      },
      "import_key": "a1533bc0-7ff5-4ea8-8a76-1565ea6af29e"
    }
  }
}"#;

#[test]
fn saved_m1_feedback_import_preserves_content_provenance_and_permanent_replay_key() {
    assert_eq!(BOARD_API, 2);
    let directory = scratch();
    let config = crate::board::BoardConfig::for_database(directory.path().join("board.sqlite3"));
    let mut backend = LocalBoard::open(&config).unwrap();
    let stored: QueuedFeedback = serde_json::from_str(SAVED_M1_FEEDBACK).unwrap();
    assert_eq!(stored.request.api, 1);
    let spool = directory.path().join("spool");
    assert_eq!(
        queue(&spool, &stored.request).unwrap_err().code,
        BoardErrorCode::BoardApiMismatch
    );
    assert!(!spool.exists());
    assert_eq!(
        backend.handle(&stored.request).unwrap_err().code,
        BoardErrorCode::BoardApiMismatch
    );
    assert_eq!(
        backend.import_feedback(&stored.request).unwrap_err().code,
        BoardErrorCode::BoardApiMismatch
    );

    fs::create_dir(&spool).unwrap();
    let path = spool.join(format!("{}.feedback", stored.import_key));
    fs::write(&path, SAVED_M1_FEEDBACK.as_bytes()).unwrap();
    let mut upgraded = stored.request.clone();
    upgraded.api = BOARD_API;
    assert_eq!(read_record(&path).unwrap(), upgraded);
    let pending = import_pending(
        &spool,
        &mut RejectedImport(BoardErrorCode::BoardUnavailable),
    )
    .unwrap();
    assert_eq!(pending.pending, 1);
    assert_eq!(pending.quarantined, 0);
    assert_eq!(fs::read(&path).unwrap(), SAVED_M1_FEEDBACK.as_bytes());

    let imported = import_pending(&spool, &mut backend).unwrap();
    assert_eq!(imported.imported, 1);
    assert_eq!(imported.quarantined, 0);
    assert!(!path.exists());
    let first_sequence = backend.max_seq().unwrap();
    fs::write(&path, SAVED_M1_FEEDBACK.as_bytes()).unwrap();
    assert_eq!(import_pending(&spool, &mut backend).unwrap().imported, 1);
    assert!(!path.exists());
    assert_eq!(backend.max_seq().unwrap(), first_sequence);
    let reply = backend
        .handle(&BoardRequest::new(
            stored.request.actor.clone(),
            BoardOp::FeedbackList { open_only: false },
        ))
        .unwrap();
    let BoardResult::Feedback(feedback) = reply.result else {
        panic!("expected stored feedback");
    };
    assert_eq!(feedback.len(), 1);
    let entry = &feedback[0].entry;
    assert_eq!(entry.actor, stored.request.actor);
    assert_eq!(entry.via, Some(FeedbackVia::Outbox));
    let BoardOp::Feedback {
        kind,
        summary,
        body,
        metadata,
        ..
    } = &stored.request.op
    else {
        panic!("saved fixture is not feedback");
    };
    assert_eq!(feedback[0].kind, *kind);
    assert_eq!(feedback[0].metadata, *metadata);
    assert_eq!(
        entry.body.as_str(),
        format!(
            "{}\n\n{}",
            summary.as_str(),
            body.as_ref().unwrap().as_str()
        )
    );
    let durable = rusqlite::Connection::open(&config.db_path).unwrap();
    let (imports, imported_entry): (u64, u64) = durable
        .query_row(
            "SELECT count(*), min(entry_id) FROM feedback_imports WHERE import_key=?1",
            [stored.import_key.to_string()],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )
        .unwrap();
    assert_eq!(imports, 1);
    assert_eq!(imported_entry, entry.id.get());
}

#[test]
fn saved_feedback_upgrade_quarantines_unknown_versions_nonfeedback_and_invalid_content() {
    let saved: serde_json::Value = serde_json::from_str(SAVED_M1_FEEDBACK).unwrap();
    let key = saved["import_key"].as_str().unwrap();
    let mut invalid_records = Vec::new();
    for api in [0, 3, 77] {
        let mut future = saved.clone();
        future["request"]["api"] = api.into();
        invalid_records.push(future);
    }
    let mut nonfeedback = saved.clone();
    nonfeedback["request"]["op"] = serde_json::json!({
        "op": "hello", "model": "m1-model", "effort": null
    });
    invalid_records.push(nonfeedback);
    let mut invalid_actor = saved.clone();
    invalid_actor["request"]["actor"]["user"] = "invalid user".into();
    invalid_records.push(invalid_actor);
    let mut oversized_body = saved.clone();
    oversized_body["request"]["op"]["body"] = "x".repeat(4097).into();
    invalid_records.push(oversized_body);
    let mut oversized_combined_text = saved.clone();
    oversized_combined_text["request"]["op"]["body"] = "x".repeat(4090).into();
    invalid_records.push(oversized_combined_text);
    let mut invalid_key = saved.clone();
    invalid_key["request"]["op"]["import_key"] = new_import_key().to_string().into();
    invalid_records.push(invalid_key);
    let mut invalid_filename_key = saved.clone();
    invalid_filename_key["import_key"] = new_import_key().to_string().into();
    invalid_records.push(invalid_filename_key);
    let mut unknown_field = saved.clone();
    unknown_field["request"]["unrecognized"] = true.into();
    invalid_records.push(unknown_field);
    for (index, record) in invalid_records.into_iter().enumerate() {
        let directory = scratch();
        let path = directory.path().join(format!("{key}.feedback"));
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        let summary = import_pending(
            directory.path(),
            &mut RejectedImport(BoardErrorCode::BoardUnavailable),
        )
        .unwrap();
        assert_eq!(summary.imported, 0, "invalid fixture {index}");
        assert_eq!(summary.pending, 0, "invalid fixture {index}");
        assert_eq!(summary.quarantined, 1, "invalid fixture {index}");
        assert!(!path.exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
