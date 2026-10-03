use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_ids::{EntryId, EventSeq};
use crate::board::board_protocol::{BoardChange, FeedbackMetadata};
use crate::board::board_vocabulary::{EntryText, FeedbackKind};
use std::collections::HashMap;

fn scratch() -> tempfile::TempDir {
    let parent = Path::new("target/test-feedback-outbox");
    fs::create_dir_all(parent).unwrap();
    tempfile::Builder::new().tempdir_in(parent).unwrap()
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
    fn max_seq(&self) -> Result<EventSeq, BoardError> {
        Ok(EventSeq::new(self.entries.len() as u64))
    }
}

#[test]
fn queue_durably_publishes_private_complete_records_without_a_database() {
    let dir = scratch();
    let spool = dir.path().join("absent-spool");
    let request = report();
    let reply = queue(&spool, &request).unwrap();
    let BoardResult::Queued { import_key } = reply.result else {
        panic!("expected queue acknowledgement");
    };
    let path = spool.join(format!("{import_key}.feedback"));
    assert_eq!(read_record(&path).unwrap(), request);
    assert_eq!(
        spool.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::read_dir(&spool).unwrap().count(), 1);
    queue(&spool, &request).unwrap();
    assert_eq!(fs::read_dir(&spool).unwrap().count(), 1);
}

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
fn queued_uuid_cannot_be_replaced_by_different_feedback() {
    let dir = scratch();
    let mut request = report();
    queue(dir.path(), &request).unwrap();
    let BoardOp::Feedback { summary, .. } = &mut request.op else {
        unreachable!()
    };
    *summary = EntryText::new("different evidence").unwrap();
    assert_eq!(
        queue(dir.path(), &request).unwrap_err().code,
        BoardErrorCode::InvalidOptions
    );
    let mut backend = ImportBackend::default();
    assert_eq!(
        import_pending(dir.path(), &mut backend).unwrap().imported,
        1
    );
}

#[test]
fn symbolic_link_records_never_read_or_modify_their_targets() {
    let dir = scratch();
    let target = dir.path().join("outside-report");
    fs::write(&target, b"private evidence").unwrap();
    let link = dir.path().join(format!("{}.feedback", new_import_key()));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut backend = ImportBackend::default();
    assert_eq!(
        import_pending(dir.path(), &mut backend)
            .unwrap()
            .quarantined,
        1
    );
    assert_eq!(fs::read(&target).unwrap(), b"private evidence");
    assert!(backend.entries.is_empty());
}

#[test]
fn invalid_reports_and_unwritable_spool_never_get_queue_acknowledgements() {
    let dir = scratch();
    let path = dir.path().join("file-instead-of-directory");
    fs::write(&path, b"existing").unwrap();
    assert_eq!(
        queue(&path, &report()).unwrap_err().code,
        BoardErrorCode::BoardUnavailable
    );
    let mut request = report();
    request.op = BoardOp::FeedbackList { open_only: true };
    assert_eq!(
        queue(dir.path(), &request).unwrap_err().code,
        BoardErrorCode::InvalidOptions
    );
}

struct RejectedImport(BoardErrorCode);

impl BoardBackend for RejectedImport {
    fn handle(&mut self, _: &BoardRequest) -> Result<BoardReply, BoardError> {
        Err(BoardError::new(self.0, "injected rejection"))
    }
    fn max_seq(&self) -> Result<EventSeq, BoardError> {
        Ok(EventSeq::new(0))
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
