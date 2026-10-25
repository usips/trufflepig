use super::super::board_backend::BoardBackend;
use super::super::board_protocol::{BoardOp, BoardRequest, FeedbackMetadata};
use super::super::board_vocabulary::{EntryText, FeedbackKind};
use super::super::{feedback_outbox, local_board::LocalBoard};
use super::*;

#[test]
fn committed_outbox_aliases_remain_idempotent_after_manual_dedupe_expiry() {
    let fixture = EdgeFixture::new();
    let spool = fixture.scratch.path().join("outbox");
    let mut board = LocalBoard::open(&fixture.config).unwrap();
    let mut request = BoardRequest::new(
        fixture.actor("codex", "offline-session"),
        BoardOp::Feedback {
            kind: FeedbackKind::Blocked,
            summary: EntryText::new("The router was unavailable").unwrap(),
            body: Some(EntryText::new("Tried board; queued the report.").unwrap()),
            plan: None,
            metadata: FeedbackMetadata {
                version: "acceptance".into(),
                ..FeedbackMetadata::default()
            },
            import_key: Some(crate::board::board_vocabulary::FeedbackImportKey::new()),
        },
    );
    feedback_outbox::queue(&spool, &request).unwrap();
    let first = std::fs::read_dir(&spool)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let first_bytes = std::fs::read(&first).unwrap();
    assert_eq!(
        feedback_outbox::import_pending(&spool, &mut board)
            .unwrap()
            .imported,
        1
    );
    if let BoardOp::Feedback { import_key, .. } = &mut request.op {
        *import_key = Some(crate::board::board_vocabulary::FeedbackImportKey::new());
    }
    feedback_outbox::queue(&spool, &request).unwrap();
    let second = std::fs::read_dir(&spool)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let second_bytes = std::fs::read(&second).unwrap();
    assert_eq!(
        feedback_outbox::import_pending(&spool, &mut board)
            .unwrap()
            .imported,
        1
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM board_feedback"),
        1,
        "manual retry should share its existing feedback entry"
    );
    Connection::open(&fixture.config.db_path)
        .unwrap()
        .execute("DELETE FROM operation_dedupes", [])
        .unwrap();
    std::fs::write(&first, first_bytes).unwrap();
    std::fs::write(&second, second_bytes).unwrap();
    let replay = feedback_outbox::import_pending(&spool, &mut board).unwrap();
    assert_eq!(replay.imported, 2);
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM board_feedback"),
        1,
        "a committed UUID became eligible after ten-minute dedupe expiry"
    );
    assert_eq!(
        board.max_seq().unwrap().get(),
        1,
        "outbox replay minted an extra event"
    );
    assert!(!first.exists() && !second.exists());
}
