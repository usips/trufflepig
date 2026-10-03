use super::*;

#[test]
fn entry_prefix_preserves_metadata_and_recomputes_cursors() {
    let primary = entry(80);
    let related = (100..104)
        .map(|seq| {
            let mut related = entry(seq);
            related.body = EntryText::new("related evidence with details ".repeat(100)).unwrap();
            related
        })
        .collect::<Vec<_>>();
    let feedback = FeedbackRecord {
        entry: primary.clone(),
        kind: crate::board::board_vocabulary::FeedbackKind::Missing,
        state: crate::board::board_vocabulary::FeedbackState::Open,
        metadata: FeedbackMetadata {
            version: "frozen-version".into(),
            cwd: "/checkout".into(),
            ..FeedbackMetadata::default()
        },
    };
    let linked_commit = review_commit(1).commit;
    let view = EntryView {
        entry: primary,
        replies: related.clone(),
        replies_omitted: 7,
        backrefs: related,
        backrefs_omitted: 9,
        replies_next_after: Some(EntryCursor {
            seq: EventSeq::new(103),
            entry: EntryId::new(103).unwrap(),
        }),
        backrefs_next_after: Some(EntryCursor {
            seq: EventSeq::new(103),
            entry: EntryId::new(103).unwrap(),
        }),
        through: EventSeq::new(110),
        proposal: None,
        feedback: Some(feedback.clone()),
        linked_commit: Some(linked_commit.clone()),
        can_decide: false,
        can_supersede: false,
        can_answer: false,
        can_triage: true,
        can_close: false,
        plan_head_revision: Some(1),
    };
    let mut reply = BoardReply::new("local", BoardResult::Entry(view));
    reply.snapshot_seq = Some(EventSeq::new(120));
    let budget = OutputBudget::new(2000).unwrap();
    let rendered = render_reply(&reply, &budget).unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let visible: EntryView = serde_json::from_value(value["result"]["data"].clone()).unwrap();
    assert_eq!(value["snapshot_seq"], 120);
    assert_eq!(visible.through, EventSeq::new(110));
    assert_eq!(visible.feedback, Some(feedback));
    assert_eq!(visible.linked_commit, Some(linked_commit));
    assert!(!visible.replies.is_empty() && visible.replies.len() < 4);
    assert!(visible.backrefs.len() < 4);
    let cursor = |rows: &[EntryRecord]| {
        rows.last().map(|row| EntryCursor {
            seq: row.seq,
            entry: row.id,
        })
    };
    assert_eq!(visible.replies_next_after, cursor(&visible.replies));
    assert_eq!(visible.backrefs_next_after, cursor(&visible.backrefs));
    assert_eq!(visible.replies_omitted, 11 - visible.replies.len());
    assert_eq!(visible.backrefs_omitted, 13 - visible.backrefs.len());
    assert_eq!(
        value["omitted"]["entries"],
        8 - visible.replies.len() - visible.backrefs.len()
    );
    assert!(budget.fits(&rendered.text));
}
