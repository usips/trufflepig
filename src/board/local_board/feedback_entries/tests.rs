use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_protocol::{BoardRequest, RecentCall};
use crate::board::board_vocabulary::EntryText;
use crate::board::local_board::LocalBoard;
use std::{fs, path::Path, time::Duration};

fn board() -> (tempfile::TempDir, LocalBoard) {
    let parent = Path::new("target/test-feedback-entries");
    fs::create_dir_all(parent).unwrap();
    let dir = tempfile::Builder::new().tempdir_in(parent).unwrap();
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
            import_key: Some(crate::board::board_vocabulary::FeedbackImportKey::new()),
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
                actor("triage"),
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
    *import_key = Some(crate::board::board_vocabulary::FeedbackImportKey::new());
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
            actor("triage"),
            BoardOp::FeedbackClose {
                entry,
                state: FeedbackState::Wontfix,
                note: None,
            },
        ))
        .unwrap();
    let error = board
        .handle(&BoardRequest::new(
            actor("other"),
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
