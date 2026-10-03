use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_protocol::{BoardRequest, RecentCall};
use crate::board::board_vocabulary::{EntryText, FeedbackImportKey};
use crate::board::local_board::LocalBoard;
use std::time::Duration;

fn board() -> (tempfile::TempDir, LocalBoard) {
    let dir = crate::board::board_test_support::scratch("board-test-");
    let board = LocalBoard::open_path(&dir.path().join("board.sqlite3"), Duration::from_secs(1800))
        .unwrap();
    (dir, board)
}

fn actor(session: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        session,
    )
    .unwrap()
}

fn human_actor(session: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("human").unwrap(),
        session,
    )
    .unwrap()
}

fn report(session: &str) -> BoardRequest {
    BoardRequest::new(
        actor(session),
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("result was incorrect").unwrap(),
            body: Some(
                EntryText::new("Expected the current source; received stale evidence.").unwrap(),
            ),
            plan: None,
            metadata: FeedbackMetadata {
                version: "0.1.0".to_owned(),
                build_id: Some("1f7e4a2".to_owned()),
                cwd: "src/board".to_owned(),
                recent_calls: vec![RecentCall {
                    verb: "show".to_owned(),
                    args: vec!["sym:write".to_owned()],
                    exit_code: Some(1),
                    error_prefix: Some("stale_source".to_owned()),
                    truncated: Some(false),
                    coverage: Some("complete".to_owned()),
                }],
                ..FeedbackMetadata::default()
            },
            import_key: Some(FeedbackImportKey::new()),
        },
    )
}

fn changed(reply: BoardReply) -> BoardChange {
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change");
    };
    change
}

#[test]
fn feedback_metadata_is_retained_and_closure_reaches_only_original_session() {
    let (_dir, mut board) = board();
    let request = report("original");
    let entry = changed(board.handle(&request).unwrap()).entry;
    let records = read_feedback(&board.conn, true).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, FeedbackKind::Wrong);
    assert_eq!(records[0].state, FeedbackState::Open);
    assert!(
        records[0]
            .entry
            .body
            .as_str()
            .contains("received stale evidence")
    );
    assert_eq!(
        records[0].metadata.recent_calls[0].error_prefix.as_deref(),
        Some("stale_source")
    );
    let closed = changed(
        board
            .handle(&BoardRequest::new(
                human_actor("triage"),
                BoardOp::FeedbackClose {
                    entry,
                    state: FeedbackState::Fixed,
                    note: Some(EntryText::new("fixed in 1f7e4a2").unwrap()),
                },
            ))
            .unwrap(),
    );
    assert_ne!(closed.entry, entry);
    assert!(read_feedback(&board.conn, true).unwrap().is_empty());
    assert_eq!(
        read_feedback(&board.conn, false).unwrap()[0].state,
        FeedbackState::Fixed
    );
    let target: String = board
        .conn
        .query_row(
            "SELECT to_whom FROM events WHERE seq=?1",
            [sqlite_id(closed.seq.get()).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(target, actor("original").identity());
    let inbox = BoardRequest::new(
        actor("original"),
        BoardOp::Inbox {
            repo_key: None,
            all: true,
            after: Some(crate::board::board_ids::EventSeq::new(closed.seq.get() - 1)),
            limit: 20,
        },
    );
    let BoardResult::Inbox(inbox) = board.handle(&inbox).unwrap().result else {
        panic!("expected inbox");
    };
    assert!(inbox.events.iter().any(|event| event.seq == closed.seq));
    assert!(
        inbox
            .events
            .iter()
            .any(|event| event.summary.as_str().contains("fixed in 1f7e4a2"))
    );
    let sibling = BoardRequest::new(
        actor("sibling-session"),
        BoardOp::Inbox {
            repo_key: None,
            all: true,
            after: Some(crate::board::board_ids::EventSeq::new(closed.seq.get() - 1)),
            limit: 20,
        },
    );
    let BoardResult::Inbox(sibling) = board.handle(&sibling).unwrap().result else {
        panic!("expected inbox");
    };
    assert!(sibling.events.iter().all(|event| event.seq != closed.seq));
}

#[test]
fn feedback_from_an_unregistered_repository_retains_its_portable_identity() {
    let (_dir, mut board) = board();
    let mut request = report("original");
    let repo = crate::board::board_ids::RepoKey::parse(&"a".repeat(40)).unwrap();
    let BoardOp::Feedback { metadata, .. } = &mut request.op else {
        unreachable!()
    };
    metadata.repo_key = Some(repo.clone());
    board.handle(&request).unwrap();
    assert_eq!(
        read_feedback(&board.conn, true).unwrap()[0]
            .metadata
            .repo_key,
        Some(repo)
    );
}

#[test]
fn permanent_alias_replay_survives_expired_content_dedupe() {
    let (_dir, mut board) = board();
    let request = report("original");
    let first = changed(board.handle(&request).unwrap());
    let mut alias = request.clone();
    let BoardOp::Feedback { import_key, .. } = &mut alias.op else {
        unreachable!()
    };
    *import_key = Some(FeedbackImportKey::new());
    assert!(changed(board.handle(&alias).unwrap()).deduplicated);
    board
        .conn
        .execute("DELETE FROM operation_dedupes", [])
        .unwrap();
    let replayed = changed(board.handle(&alias).unwrap());
    assert!(replayed.deduplicated);
    assert_eq!(replayed.entry, first.entry);
    assert_eq!(board.max_seq().unwrap(), first.seq);
    assert_eq!(read_feedback(&board.conn, false).unwrap().len(), 1);
}

#[test]
fn oversized_recent_calls_and_combined_body_are_rejected_before_mutation() {
    let (_dir, mut board) = board();
    let mut request = report("original");
    let BoardOp::Feedback { metadata, .. } = &mut request.op else {
        unreachable!()
    };
    metadata.recent_calls[0].args = vec!["x".repeat(2048)];
    assert!(board.handle(&request).is_err());
    let mut request = report("original");
    let BoardOp::Feedback { body, .. } = &mut request.op else {
        unreachable!()
    };
    *body = Some(EntryText::new("x".repeat(4096)).unwrap());
    assert!(board.handle(&request).is_err());
    assert_eq!(board.max_seq().unwrap().get(), 0);
}

#[test]
fn terminal_feedback_cannot_be_changed_to_another_terminal_state() {
    let (_dir, mut board) = board();
    let entry = changed(board.handle(&report("original")).unwrap()).entry;
    board
        .conn
        .execute(
            "UPDATE entries SET state='triaged' WHERE id=?1",
            [sqlite_id(entry.get()).unwrap()],
        )
        .unwrap();
    board
        .handle(&BoardRequest::new(
            human_actor("triage"),
            BoardOp::FeedbackClose {
                entry,
                state: FeedbackState::Wontfix,
                note: None,
            },
        ))
        .unwrap();
    let error = board
        .handle(&BoardRequest::new(
            human_actor("other"),
            BoardOp::FeedbackClose {
                entry,
                state: FeedbackState::Duplicate,
                note: None,
            },
        ))
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidState
    );
    assert_eq!(
        read_feedback(&board.conn, false).unwrap()[0].state,
        FeedbackState::Wontfix
    );
}

#[test]
fn feedback_content_dedupe_ignores_session_and_observation_metadata() {
    let (_dir, mut board) = board();
    let first = changed(board.handle(&report("original")).unwrap());
    let mut replay = report("restarted");
    let BoardOp::Feedback { metadata, .. } = &mut replay.op else {
        unreachable!()
    };
    metadata.cwd = "another/subdirectory".into();
    metadata.recent_calls.clear();
    metadata.steer_mode = Some("off".into());
    let replay = changed(board.handle(&replay).unwrap());
    assert!(replay.deduplicated);
    assert_eq!(replay.entry, first.entry);
    assert_eq!(board.max_seq().unwrap(), first.seq);
}

#[test]
fn feedback_triage_and_close_require_owner_authority() {
    let (_dir, mut board) = board();
    let entry = changed(board.handle(&report("original")).unwrap()).entry;
    for unauthorized in [
        actor("untrusted"),
        BoardActor::new(
            "other",
            "laptop",
            HarnessLabel::parse("human").unwrap(),
            "human",
        )
        .unwrap(),
    ] {
        let error = board
            .handle(&BoardRequest::new(
                unauthorized,
                BoardOp::FeedbackClose {
                    entry,
                    state: FeedbackState::Fixed,
                    note: None,
                },
            ))
            .unwrap_err();
        assert_eq!(
            error.code,
            crate::board::board_protocol::BoardErrorCode::InvalidActor
        );
    }
    board
        .handle(&BoardRequest::new(
            human_actor("triage"),
            BoardOp::FeedbackTriage {
                entry,
                note: Some(EntryText::new("confirmed reproduction").unwrap()),
            },
        ))
        .unwrap();
    assert_eq!(
        read_feedback(&board.conn, true).unwrap()[0].state,
        FeedbackState::Triaged
    );
    board
        .handle(&BoardRequest::new(
            human_actor("close"),
            BoardOp::FeedbackClose {
                entry,
                state: FeedbackState::Fixed,
                note: None,
            },
        ))
        .unwrap();
    assert!(read_feedback(&board.conn, true).unwrap().is_empty());
}

#[test]
fn plan_feedback_requires_the_owners_human_or_steward() {
    let (_dir, mut board) = board();
    let plan = changed(
        board
            .handle(&BoardRequest::new(
                actor("owner"),
                BoardOp::New {
                    title: crate::board::board_vocabulary::PlanTitle::new("Feedback scope")
                        .unwrap(),
                    body: crate::board::board_vocabulary::PlanText::new("").unwrap(),
                    steward: Some(HarnessLabel::parse("claude").unwrap()),
                },
            ))
            .unwrap(),
    )
    .plan
    .unwrap();
    let mut request = report("reporter");
    let BoardOp::Feedback {
        plan: report_plan, ..
    } = &mut request.op
    else {
        unreachable!()
    };
    *report_plan = Some(plan);
    let entry = changed(board.handle(&request).unwrap()).entry;
    let other_user = BoardActor::new(
        "other",
        "laptop",
        HarnessLabel::parse("claude").unwrap(),
        "steward",
    )
    .unwrap();
    assert!(
        board
            .handle(&BoardRequest::new(
                other_user,
                BoardOp::FeedbackTriage { entry, note: None }
            ))
            .is_err()
    );
    let steward = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("claude").unwrap(),
        "steward",
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            steward,
            BoardOp::FeedbackTriage { entry, note: None },
        ))
        .unwrap();
}

#[test]
fn populated_m1_feedback_uuid_replay_survives_schema_and_dedupe_upgrade() {
    let directory = crate::board::board_test_support::scratch("board-test-");
    let path = directory.path().join("legacy-feedback.sqlite3");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    legacy
        .execute_batch(crate::board::local_board::board_database::legacy_schema())
        .unwrap();
    let key = FeedbackImportKey::parse("aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa").unwrap();
    legacy.execute_batch(r#"
        INSERT INTO actors VALUES(1,'josh','laptop','codex','original');
        INSERT INTO agent_sessions VALUES(1,NULL,NULL,NULL,NULL,10,10);
        INSERT INTO entries VALUES(1,NULL,'feedback','M1 report',NULL,NULL,1,NULL,NULL,NULL,'open','legacy-request-hash',1,10);
        INSERT INTO board_feedback VALUES(1,'wrong','0.1.0',NULL,'src',NULL,'[]','aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa');
        INSERT INTO feedback_imports VALUES('aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa',1);
        INSERT INTO events VALUES(1,NULL,'feedback','E1',NULL,1,'M1 report',10);
        INSERT INTO operation_dedupes VALUES('legacy-request-hash','{"api":1,"backend":"local","warnings":[],"result":{"result":"change","data":{"entry":"E1","seq":1,"plan":null,"revision":null,"task":null,"deduplicated":false}}}',10);
        PRAGMA user_version=1;
    "#).unwrap();
    drop(legacy);
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(1800)).unwrap();
    let request = BoardRequest::new(
        actor("restarted"),
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("M1 report").unwrap(),
            body: None,
            plan: None,
            metadata: FeedbackMetadata::default(),
            import_key: Some(key),
        },
    );
    let reply = board.handle(&request).unwrap();
    assert_eq!(reply.api, crate::board::board_protocol::BOARD_API);
    let replayed = changed(reply);
    assert!(replayed.deduplicated);
    assert_eq!(
        replayed.entry,
        crate::board::board_ids::EntryId::new(1).unwrap()
    );
    assert_eq!(replayed.seq.get(), 1);
    assert_eq!(board.max_seq().unwrap().get(), 1);
    assert_eq!(read_feedback(&board.conn, false).unwrap().len(), 1);
    let hashes: Vec<String> = board
        .conn
        .prepare("SELECT dedupe_key FROM operation_dedupes")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(hashes, ["legacy-request-hash"]);
}
