mod board_import_tests;
mod board_spool_tests;
mod stored_feedback_upgrade_tests;

use super::board_spool::private_directory_owned_by;
use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_ids::{EntryId, EventSeq};
use crate::board::board_protocol::{BoardChange, FeedbackMetadata};
use crate::board::board_vocabulary::{EntryText, FeedbackKind};
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn scratch() -> tempfile::TempDir {
    crate::board::board_test_support::scratch("feedback-outbox-")
}

fn report() -> BoardRequest {
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "original-session",
    )
    .unwrap();
    BoardRequest::new(
        actor,
        BoardOp::Feedback {
            kind: FeedbackKind::Blocked,
            summary: EntryText::new("daemon unavailable").unwrap(),
            body: Some(EntryText::new("Tried search; used a source read instead.").unwrap()),
            plan: None,
            metadata: FeedbackMetadata::default(),
            import_key: Some(new_import_key()),
        },
    )
}

#[derive(Default)]
struct ImportBackend {
    unavailable: bool,
    entries: HashMap<String, BoardChange>,
}

impl BoardBackend for ImportBackend {
    fn handle(&mut self, request: &BoardRequest) -> Result<BoardReply, BoardError> {
        if self.unavailable {
            return Err(BoardError::new(
                BoardErrorCode::BoardUnavailable,
                "database unavailable",
            ));
        }
        let BoardOp::Feedback {
            import_key: Some(key),
            ..
        } = &request.op
        else {
            panic!("outbox dispatched another operation");
        };
        let next = self.entries.len() as u64 + 1;
        let change = self
            .entries
            .entry(key.to_string())
            .or_insert_with(|| BoardChange {
                entry: EntryId::new(next).unwrap(),
                seq: EventSeq::new(next),
                plan: None,
                revision: None,
                task: None,
                deduplicated: false,
            })
            .clone();
        Ok(BoardReply::new("test", BoardResult::Change(change)))
    }
    fn import_feedback(&mut self, request: &BoardRequest) -> Result<BoardReply, BoardError> {
        self.handle(request)
    }
    fn max_seq(&self) -> Result<EventSeq, BoardError> {
        Ok(EventSeq::new(self.entries.len() as u64))
    }
}

struct RejectedImport(BoardErrorCode);

impl BoardBackend for RejectedImport {
    fn handle(&mut self, _: &BoardRequest) -> Result<BoardReply, BoardError> {
        Err(BoardError::new(self.0, "injected rejection"))
    }
    fn import_feedback(&mut self, request: &BoardRequest) -> Result<BoardReply, BoardError> {
        self.handle(request)
    }
    fn max_seq(&self) -> Result<EventSeq, BoardError> {
        Ok(EventSeq::new(0))
    }
}

struct SqliteRejectedImport {
    raw_code: i32,
    attempts: usize,
}

impl BoardBackend for SqliteRejectedImport {
    fn handle(&mut self, _: &BoardRequest) -> Result<BoardReply, BoardError> {
        self.attempts += 1;
        let sqlite = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(self.raw_code),
            Some("simulated storage failure".into()),
        );
        Err(BoardError::from(
            anyhow::Error::new(sqlite).context("import feedback"),
        ))
    }
    fn import_feedback(&mut self, request: &BoardRequest) -> Result<BoardReply, BoardError> {
        self.handle(request)
    }
    fn max_seq(&self) -> Result<EventSeq, BoardError> {
        Ok(EventSeq::new(0))
    }
}
