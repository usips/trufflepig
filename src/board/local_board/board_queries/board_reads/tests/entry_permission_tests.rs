use super::*;

fn view(board: &mut LocalBoard, actor: BoardActor, entry: EntryId) -> EntryView {
    let BoardResult::Entry(view) = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::Show {
                target: BoardRef::Entry(entry),
            },
        ))
        .unwrap()
        .result
    else {
        panic!("expected entry detail");
    };
    view
}

#[test]
fn entry_feedback_detail_preserves_metadata_and_shared_management_authority() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let reporter = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "reporter",
    )
    .unwrap();
    let human = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("human").unwrap(),
        "owner",
    )
    .unwrap();
    let foreign = BoardActor::new(
        "other",
        "laptop",
        HarnessLabel::parse("human").unwrap(),
        "foreign",
    )
    .unwrap();
    let metadata = FeedbackMetadata {
        version: "fixture-version".into(),
        build_id: Some("fixture-build".into()),
        cwd: "src/board".into(),
        steer_mode: Some("strict".into()),
        ..FeedbackMetadata::default()
    };
    for linked_plan in [Some(plan), None] {
        let reply = board
            .import_feedback(&BoardRequest::new(
                reporter.clone(),
                BoardOp::Feedback {
                    kind: FeedbackKind::Missing,
                    summary: EntryText::new("missing detail").unwrap(),
                    body: Some(EntryText::new("reproduction steps").unwrap()),
                    plan: linked_plan,
                    metadata: metadata.clone(),
                    import_key: Some(crate::board::board_vocabulary::FeedbackImportKey::new()),
                },
            ))
            .unwrap();
        let BoardResult::Change(change) = reply.result else {
            panic!("expected feedback");
        };
        let owner_view = view(&mut board, human.clone(), change.entry);
        assert_eq!(owner_view.feedback.as_ref().unwrap().metadata, metadata);
        assert_eq!(owner_view.entry.via, Some(FeedbackVia::Outbox));
        assert!(owner_view.can_triage && owner_view.can_close);
        assert!(owner_view.linked_commit.is_none());
        let reporter_view = view(&mut board, reporter.clone(), change.entry);
        assert!(!reporter_view.can_triage && !reporter_view.can_close);
        let foreign_view = view(&mut board, foreign.clone(), change.entry);
        assert!(!foreign_view.can_triage && !foreign_view.can_close);
        assert_eq!(foreign_view.feedback.as_ref().unwrap().metadata, metadata);
        board
            .handle(&BoardRequest::new(
                human.clone(),
                BoardOp::FeedbackTriage {
                    entry: change.entry,
                    note: None,
                },
            ))
            .unwrap();
        let triaged = view(&mut board, human.clone(), change.entry);
        assert!(!triaged.can_triage && triaged.can_close);
        assert_eq!(
            triaged.feedback.as_ref().unwrap().state,
            crate::board::board_vocabulary::FeedbackState::Triaged
        );
    }
}
