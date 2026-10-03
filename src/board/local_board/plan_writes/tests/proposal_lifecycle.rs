use super::*;

fn proposal_op(base: PlanRevision, summary: &str, supersedes: Option<EntryId>) -> BoardOp {
    let op = BoardOp::Propose {
        supersedes: None,
        base,
        body: PlanText::new(format!("body for {summary}")).unwrap(),
        summary: EntryText::new(summary).unwrap(),
    };
    let mut value = serde_json::to_value(op).unwrap();
    if let Some(previous) = supersedes {
        value["supersedes"] = serde_json::to_value(previous).unwrap();
    }
    serde_json::from_value(value).unwrap()
}

fn review(board: &mut LocalBoard, plan: PlanId) -> ReviewEvidence {
    let result = board
        .handle(&BoardRequest::new(
            actor("josh", "human", "owner"),
            BoardOp::Review {
                base: PlanRevision::new(plan, 1).unwrap(),
                agent: None,
            },
        ))
        .unwrap()
        .result;
    match result {
        BoardResult::Review(evidence) => evidence,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn superseding_proposals_preserve_history_and_close_only_the_previous_proposal() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let base = PlanRevision::new(plan, 1).unwrap();
    let proposer = actor("josh", "codex", "proposer");
    let previous = call(
        &mut board,
        proposer.clone(),
        proposal_op(base, "first", None),
    );
    let before = snapshot(&board, plan);
    let replacement = call(
        &mut board,
        proposer.clone(),
        proposal_op(base, "replacement", Some(previous.entry)),
    );
    assert_eq!(snapshot(&board, plan).0, before.0);
    assert_eq!(replacement.seq.get(), previous.seq.get() + 1);
    let old = read_entry(&board.conn, previous.entry).unwrap();
    assert_eq!(
        serde_json::to_value(&old).unwrap()["state"]["state"],
        "superseded"
    );
    assert_eq!(old.body.as_str(), "first");
    let new = read_entry(&board.conn, replacement.entry).unwrap();
    assert_eq!(new.supersedes, Some(previous.entry));
    assert_eq!(
        serde_json::to_value(&new).unwrap()["state"]["state"],
        "open"
    );
    let evidence = review(&mut board, plan);
    assert_eq!(evidence.head.body.as_str(), "# Scope\noriginal");
    assert_eq!(evidence.open_proposals.len(), 1);
    assert_eq!(evidence.open_proposals[0].entry, replacement.entry);
    assert_eq!(
        evidence.open_proposals[0].body.as_str(),
        "body for replacement"
    );
    let old_body: String = board
        .conn
        .query_row(
            "SELECT t.body FROM proposals p JOIN texts t ON t.hash=p.text_hash WHERE p.entry_id=?1",
            [sql_number(previous.entry.get())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_body, "body for first");
    let before = snapshot(&board, plan);
    let error = board
        .handle(&BoardRequest::new(
            proposer,
            proposal_op(base, "second replacement", Some(previous.entry)),
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidState);
    assert_eq!(snapshot(&board, plan), before);
}

#[test]
fn superseding_proposals_reject_invalid_targets_without_any_mutation() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let base = PlanRevision::new(plan, 1).unwrap();
    let proposer = actor("josh", "codex", "proposer");
    let own = call(&mut board, proposer.clone(), proposal_op(base, "own", None));
    let other = call(
        &mut board,
        actor("other", "codex", "proposer"),
        proposal_op(base, "other author", None),
    );
    let other_session = call(
        &mut board,
        actor("josh", "codex", "other-session"),
        proposal_op(base, "other session", None),
    );
    let other_plan = call(
        &mut board,
        actor("josh", "human", "owner"),
        BoardOp::New {
            title: PlanTitle::new("Other plan").unwrap(),
            body: PlanText::new("other plan body").unwrap(),
            steward: None,
        },
    )
    .plan
    .unwrap();
    let cross_plan = call(
        &mut board,
        proposer.clone(),
        proposal_op(
            PlanRevision::new(other_plan, 1).unwrap(),
            "cross plan",
            None,
        ),
    );
    call(
        &mut board,
        actor("josh", "human", "owner"),
        BoardOp::Reject {
            proposal: own.entry,
            reason: EntryText::new("closed").unwrap(),
        },
    );
    for (previous, expected) in [
        (EntryId::new(999).unwrap(), BoardErrorCode::InvalidReference),
        (cross_plan.entry, BoardErrorCode::InvalidReference),
        (other.entry, BoardErrorCode::InvalidActor),
        (other_session.entry, BoardErrorCode::InvalidActor),
        (own.entry, BoardErrorCode::InvalidState),
    ] {
        let before = snapshot(&board, plan);
        let error = board
            .handle(&BoardRequest::new(
                proposer.clone(),
                proposal_op(base, &format!("invalid {previous}"), Some(previous)),
            ))
            .unwrap_err();
        assert_eq!(error.code, expected);
        assert_eq!(snapshot(&board, plan), before);
    }
}

#[test]
fn proposal_stale_base_tracks_the_head_and_rebased_supersession() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let base = PlanRevision::new(plan, 1).unwrap();
    let proposer = actor("josh", "codex", "proposer");
    let stale = call(
        &mut board,
        proposer.clone(),
        proposal_op(base, "later rebase", None),
    );
    let accepted = call(
        &mut board,
        actor("josh", "muse", "other"),
        proposal_op(base, "advance head", None),
    );
    let initial = review(&mut board, plan);
    for proposal in initial.open_proposals {
        assert_eq!(serde_json::to_value(proposal).unwrap()["stale_base"], false);
    }
    call(
        &mut board,
        actor("josh", "human", "owner"),
        BoardOp::Accept {
            proposal: accepted.entry,
            note: None,
        },
    );
    let evidence = review(&mut board, plan);
    assert_eq!(evidence.open_proposals.len(), 1);
    assert_eq!(
        serde_json::to_value(&evidence.open_proposals[0]).unwrap()["stale_base"],
        true
    );
    let before = snapshot(&board, plan);
    let error = board
        .handle(&BoardRequest::new(
            actor("josh", "human", "owner"),
            BoardOp::Accept {
                proposal: stale.entry,
                note: None,
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::StaleRevision);
    assert_eq!(snapshot(&board, plan), before);
    let rebased = call(
        &mut board,
        proposer,
        proposal_op(
            PlanRevision::new(plan, 2).unwrap(),
            "rebased",
            Some(stale.entry),
        ),
    );
    let evidence = review(&mut board, plan);
    assert_eq!(evidence.open_proposals.len(), 1);
    assert_eq!(evidence.open_proposals[0].entry, rebased.entry);
    assert_eq!(
        serde_json::to_value(&evidence.open_proposals[0]).unwrap()["stale_base"],
        false
    );
}

#[test]
fn proposal_grammar_accepts_supersedes_and_edit_rejects_it() {
    let args: Vec<String> = [
        "board",
        "propose",
        "P1@1",
        "--supersedes",
        "E2",
        "--body",
        "plan.md",
        "replacement",
    ]
    .map(str::to_owned)
    .to_vec();
    let options = crate::cli::parse(&args).unwrap();
    let command = crate::board::board_grammar::parse(&options, Some("replacement body")).unwrap();
    let crate::board::board_grammar::BoardCommand::Op(op) = command else {
        panic!("expected op");
    };
    assert_eq!(serde_json::to_value(op).unwrap()["supersedes"], "E2");
    let mut edit_options = options;
    edit_options.words[1] = "edit".to_owned();
    let error =
        crate::board::board_grammar::parse(&edit_options, Some("replacement body")).unwrap_err();
    assert!(error.to_string().starts_with("invalid_options:"));
}
