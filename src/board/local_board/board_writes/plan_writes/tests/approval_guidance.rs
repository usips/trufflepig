use super::*;

#[test]
fn plan_creator_without_steward_gets_guidance_but_no_approval_authority() {
    let (_directory, mut board) = database();
    let creator = actor("josh", "claude", "creator");
    let reply = board
        .handle(&BoardRequest::new(
            creator.clone(),
            BoardOp::New {
                title: PlanTitle::new("Explicit authority").unwrap(),
                body: PlanText::new("original").unwrap(),
                steward: None,
                repo_key: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected created plan");
    };
    let plan = change.plan.unwrap();
    let warning = reply.warnings.join(" ");
    assert!(warning.contains(&format!("{plan} has no steward")));
    assert!(warning.contains("owner josh acting as human"));
    assert!(warning.contains("board new --steward claude"));

    let proposal = call(
        &mut board,
        creator.clone(),
        BoardOp::Propose {
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("replacement").unwrap(),
            summary: EntryText::new("proposed change").unwrap(),
            supersedes: None,
        },
    );
    let before = snapshot(&board, plan);
    let op = BoardOp::Accept {
        proposal: proposal.entry,
        note: None,
    };
    let error = board
        .handle(&BoardRequest::new(creator, op.clone()))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidActor);
    assert!(error.message.contains("no steward is assigned"));
    assert!(error.message.contains("ask owner josh acting as human"));
    assert_eq!(snapshot(&board, plan), before);
    let accepted = call(&mut board, actor("josh", "human", "owner"), op);
    assert_eq!(accepted.revision.unwrap().revision, 2);
}

#[test]
fn plan_with_explicit_steward_has_no_missing_steward_warning() {
    let (_directory, mut board) = database();
    let reply = board
        .handle(&BoardRequest::new(
            actor("josh", "claude", "creator"),
            BoardOp::New {
                title: PlanTitle::new("Delegated authority").unwrap(),
                body: PlanText::new("").unwrap(),
                steward: Some(HarnessLabel::parse("claude").unwrap()),
                repo_key: None,
            },
        ))
        .unwrap();
    assert!(reply.warnings.is_empty());
}
