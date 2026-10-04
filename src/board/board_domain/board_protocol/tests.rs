use super::*;
use crate::board::{
    board_actor::{BoardActor, HarnessLabel},
    board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision, TaskId},
    board_vocabulary::{EntryKind, EntryText, FeedbackState},
};

#[test]
fn git_coauthor_actor_round_trips_as_evidence_but_cannot_request() {
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("git:alice@example.com").unwrap(),
        "git-evidence",
    )
    .unwrap();
    let encoded = serde_json::to_string(&actor).unwrap();
    assert_eq!(serde_json::from_str::<BoardActor>(&encoded).unwrap(), actor);
    let request = BoardRequest::new(
        actor,
        BoardOp::Show {
            target: BoardRef::Plan(PlanId::new(1).unwrap()),
        },
    );
    assert!(
        request
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_actor:")
    );
}

#[test]
fn versioned_requests_round_trip_and_reject_mismatch() {
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "c1",
    )
    .unwrap();
    let mut request = BoardRequest::new(
        actor,
        BoardOp::Inbox {
            after: Some(EventSeq::new(0)),
            limit: 20,
            repo_key: None,
            all: true,
        },
    );
    let decoded: BoardRequest =
        serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
    assert_eq!(decoded, request);
    decoded.validate().unwrap();
    request.api += 1;
    assert!(
        request
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("board_api_mismatch:")
    );
}

#[test]
fn internal_entry_kinds_and_open_feedback_cannot_be_posted_or_closed() {
    let post = BoardOp::Post {
        target: BoardRef::Plan(PlanId::new(1).unwrap()),
        kind: EntryKind::Claim,
        body: EntryText::new("scope").unwrap(),
        to: None,
        supersedes: None,
    };
    assert!(
        post.validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_kind:")
    );
    let close = BoardOp::FeedbackClose {
        entry: EntryId::new(1).unwrap(),
        state: FeedbackState::Open,
        note: None,
    };
    assert!(
        close
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_state:")
    );
}

#[test]
fn claim_scope_is_required_unless_resuming() {
    for resume in [false, true] {
        let op: BoardOp = serde_json::from_value(serde_json::json!({
            "op": "claim_task", "task": "P1.1", "scope": null, "resume": resume
        }))
        .unwrap();
        assert_eq!(op.validate().is_ok(), resume);
    }
}

#[test]
fn public_address_records_are_revalidated_at_the_request_boundary() {
    let plan = PlanId::new(1).unwrap();
    let bad_revision = BoardOp::Review {
        base: PlanRevision { plan, revision: 0 },
        agent: None,
    };
    let bad_task = BoardOp::ClaimTask {
        task: TaskId {
            plan,
            ordinal: u64::MAX,
        },
        scope: Some(EntryText::new("owned scope").unwrap()),
        resume: false,
    };
    assert!(bad_revision.validate().is_err());
    assert!(bad_task.validate().is_err());
}

#[test]
fn feedback_enforces_recent_call_and_metadata_bounds() {
    let call = RecentCall {
        verb: "search".into(),
        args: vec!["symbol".into()],
        exit_code: Some(2),
        error_prefix: Some("unavailable".into()),
        truncated: None,
        coverage: None,
    };
    let mut metadata = FeedbackMetadata {
        recent_calls: vec![call; 6],
        ..FeedbackMetadata::default()
    };
    assert!(metadata.validate().is_err());
    metadata.recent_calls.truncate(5);
    metadata.validate().unwrap();
    metadata.build_id = Some("x".repeat(257));
    assert!(metadata.validate().is_err());
    metadata.build_id = None;
    metadata.recent_calls[0].args = vec!["x".repeat(2048)];
    assert!(metadata.validate().is_err());
}

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
fn sqlite_storage_failures_classify_by_typed_code() {
    for (raw, expected) in [
        (rusqlite::ffi::SQLITE_BUSY, BoardErrorCode::DatabaseLocked),
        (rusqlite::ffi::SQLITE_LOCKED, BoardErrorCode::DatabaseLocked),
        (rusqlite::ffi::SQLITE_CONSTRAINT, BoardErrorCode::InvalidState),
        (
            rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
            BoardErrorCode::InvalidState,
        ),
        (
            rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
            BoardErrorCode::InvalidState,
        ),
        (rusqlite::ffi::SQLITE_CORRUPT, BoardErrorCode::InvalidState),
        (rusqlite::ffi::SQLITE_NOTADB, BoardErrorCode::InvalidState),
        (rusqlite::ffi::SQLITE_IOERR, BoardErrorCode::BoardUnavailable),
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

mod read_collections_tests;
