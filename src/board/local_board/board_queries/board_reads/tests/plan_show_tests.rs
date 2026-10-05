use super::*;

#[test]
fn show_revisions_and_ranges_preserves_ssot_and_proposal_state() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let base = PlanRevision::new(plan, 1).unwrap();
    let proposal = match call(
        &mut board,
        "codex",
        BoardOp::Propose {
            supersedes: None,
            base,
            body: PlanText::new("# Updated").unwrap(),
            summary: EntryText::new("Clarify scope").unwrap(),
        },
    ) {
        BoardResult::Change(change) => change.entry,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(
        entry(&board.conn, proposal).unwrap().state,
        Some(EntryState::Proposal(ProposalState::Open))
    );
    call(
        &mut board,
        "human",
        BoardOp::Accept {
            proposal,
            note: None,
        },
    );
    assert_eq!(
        entry(&board.conn, proposal).unwrap().state,
        Some(EntryState::Proposal(ProposalState::Accepted))
    );
    let BoardResult::Diff(diff) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: BoardRef::Span(RevisionSpan {
                plan,
                start: 1,
                end: None,
            }),
        },
    ) else {
        panic!("missing diff")
    };
    assert_eq!(diff.before.id, base);
    assert_eq!(diff.after.id.revision, 2);
    assert!(diff.before.body.as_str().contains("Uncovered"));
    assert_eq!(diff.after.body.as_str(), "# Updated");
    let BoardResult::Revision(first) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: BoardRef::Revision(base),
        },
    ) else {
        panic!("missing revision")
    };
    assert_eq!(first, diff.before);
}
