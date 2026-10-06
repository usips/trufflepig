use super::*;

#[test]
fn feedback_outcome_keeps_the_to_whom_gate() {
    use crate::board::board_protocol::FeedbackMetadata;
    use crate::board::board_vocabulary::{FeedbackKind, FeedbackState};
    let (_directory, mut board) = database();
    let BoardResult::Change(report) = call(
        &mut board,
        "codex",
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("stray report").unwrap(),
            body: None,
            plan: None,
            metadata: FeedbackMetadata::default(),
            import_key: None,
        },
    ) else {
        panic!("missing report");
    };
    call(
        &mut board,
        "human",
        BoardOp::FeedbackClose {
            entry: report.entry,
            state: FeedbackState::Fixed,
            note: None,
        },
    );
    // An outcome addressed to someone specific is not the report author's news.
    board
        .conn
        .execute(
            "UPDATE events SET to_whom='muse' WHERE kind='feedback' AND subject=?1",
            [report.entry.to_string()],
        )
        .unwrap();
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "session2",
    )
    .unwrap();
    let BoardResult::Inbox(inbox) = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::Inbox {
                after: Some(EventSeq::new(0)),
                limit: 100,
                repo_key: None,
                all: true,
            },
        ))
        .unwrap()
        .result
    else {
        panic!("missing inbox");
    };
    assert!(
        !inbox
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Feedback
                && event.subject == BoardRef::Entry(report.entry)),
        "an outcome addressed elsewhere skips the report author's inbox"
    );
}
