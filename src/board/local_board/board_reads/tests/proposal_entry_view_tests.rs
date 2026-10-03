use super::*;

#[test]
fn show_entry_recovers_full_large_proposal_and_keeps_decided_evidence() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let body = "proposal body with concrete evidence\n".repeat(850);
    assert!((30_000..32_768).contains(&body.len()));
    let BoardResult::Change(proposed) = call(
        &mut board,
        "codex",
        BoardOp::Propose {
            supersedes: None,
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new(body.clone()).unwrap(),
            summary: EntryText::new("compact proposal summary").unwrap(),
        },
    ) else {
        panic!("missing proposal");
    };
    let BoardResult::Entry(view) = call(
        &mut board,
        "human",
        BoardOp::Show {
            target: Some(BoardRef::Entry(proposed.entry)),
        },
    ) else {
        panic!("missing entry view");
    };
    assert_eq!(view.proposal.as_ref().unwrap().body.as_str(), body);
    assert!(view.can_decide);
    assert_eq!(view.plan_head_revision, Some(1));
    let rendered = crate::board::board_render::render_reply(
        &BoardReply::new("local", BoardResult::Entry(view)),
        &crate::output::OutputBudget::new(32768)
            .unwrap()
            .with_format(crate::output::OutputFormat::Json),
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    assert_eq!(json["result"]["data"]["proposal"]["body"], body);
    call(
        &mut board,
        "human",
        BoardOp::Accept {
            proposal: proposed.entry,
            note: None,
        },
    );
    let BoardResult::Entry(decided) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: Some(BoardRef::Entry(proposed.entry)),
        },
    ) else {
        panic!("missing decided proposal");
    };
    assert_eq!(decided.proposal.as_ref().unwrap().body.as_str(), body);
    assert_eq!(
        decided.proposal.as_ref().unwrap().state,
        ProposalState::Accepted
    );
    assert!(!decided.can_decide);
    assert!(decided.can_supersede);
    let question = post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "question to answer",
    );
    let answer = post(
        &mut board,
        "codex",
        plan,
        EntryKind::Answer,
        &format!("answer {question}"),
    );
    let BoardResult::Entry(question) = call(
        &mut board,
        "human",
        BoardOp::Show {
            target: Some(BoardRef::Entry(question)),
        },
    ) else {
        panic!("missing question");
    };
    assert_eq!(question.replies[0].id, answer);
    assert!(question.backrefs.iter().any(|entry| entry.id == answer));
    assert!(!question.can_answer);
}
