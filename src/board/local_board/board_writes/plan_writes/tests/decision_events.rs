use super::*;
use crate::board::board_actor::BoardRecipient;

#[test]
fn proposal_decisions_broadcast_to_author_and_other_participants() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let proposer = actor("josh", "codex", "proposer");
    let accepted = call(
        &mut board,
        proposer.clone(),
        BoardOp::Propose {
            supersedes: None,
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("accepted replacement").unwrap(),
            summary: EntryText::new("replacement proposal").unwrap(),
        },
    );
    call(
        &mut board,
        actor("josh", "human", "owner"),
        BoardOp::Accept {
            proposal: accepted.entry,
            note: Some(EntryText::new("approved scope").unwrap()),
        },
    );
    let rejected = call(
        &mut board,
        proposer.clone(),
        BoardOp::Propose {
            supersedes: None,
            base: PlanRevision::new(plan, 2).unwrap(),
            body: PlanText::new("rejected replacement").unwrap(),
            summary: EntryText::new("second replacement proposal").unwrap(),
        },
    );
    call(
        &mut board,
        actor("josh", "human", "owner"),
        BoardOp::Reject {
            proposal: rejected.entry,
            reason: EntryText::new("outside scope").unwrap(),
        },
    );
    call(
        &mut board,
        actor("josh", "human", "owner"),
        BoardOp::Post {
            target: BoardRef::Plan(plan),
            kind: EntryKind::Note,
            body: EntryText::new("private note for proposer").unwrap(),
            to: Some(BoardRecipient::parse("codex").unwrap()),
            supersedes: None,
        },
    );
    for participant in [proposer.clone(), actor("other", "muse", "observer")] {
        let is_author = participant == proposer;
        let result = board
            .handle(&BoardRequest::new(
                participant,
                BoardOp::Inbox {
                    repo_key: None,
                    all: true,
                    after: Some(EventSeq::new(0)),
                    limit: 100,
                },
            ))
            .unwrap()
            .result;
        let BoardResult::Inbox(inbox) = result else {
            panic!("expected inbox");
        };
        let decisions: Vec<_> = inbox
            .events
            .iter()
            .filter(|event| matches!(event.kind, EntryKind::Accept | EntryKind::Reject))
            .collect();
        assert_eq!(decisions.len(), 2);
        for decision in &decisions {
            assert_eq!(decision.to, None);
            assert!(decision.summary.as_str().contains(&proposer.identity()));
        }
        assert!(decisions[0].summary.as_str().contains("approved scope"));
        assert!(decisions[1].summary.as_str().contains("outside scope"));
        assert_eq!(
            inbox
                .events
                .iter()
                .any(|event| event.summary.as_str() == "private note for proposer"),
            is_author
        );
    }
}

#[test]
fn proposal_decisions_preserve_maximum_bodies_and_bound_broadcast_summaries() {
    for detail in ["x".repeat(4096), "é".repeat(2048)] {
        for accept in [true, false] {
            let (_directory, mut board) = database();
            let plan = plan(&mut board);
            let proposer = actor("josh", "codex", "proposer");
            let proposal = call(
                &mut board,
                proposer.clone(),
                BoardOp::Propose {
                    supersedes: None,
                    base: PlanRevision::new(plan, 1).unwrap(),
                    body: PlanText::new("replacement").unwrap(),
                    summary: EntryText::new("proposal").unwrap(),
                },
            );
            let op = if accept {
                BoardOp::Accept {
                    proposal: proposal.entry,
                    note: Some(EntryText::new(detail.clone()).unwrap()),
                }
            } else {
                BoardOp::Reject {
                    proposal: proposal.entry,
                    reason: EntryText::new(detail.clone()).unwrap(),
                }
            };
            let decision = call(&mut board, actor("josh", "human", "owner"), op);
            assert_eq!(
                read_entry(&board.conn, decision.entry)
                    .unwrap()
                    .body
                    .as_str(),
                detail
            );
            let result = board
                .handle(&BoardRequest::new(
                    actor("other", "muse", "observer"),
                    BoardOp::Inbox {
                        repo_key: None,
                        all: true,
                        after: Some(EventSeq::new(0)),
                        limit: 100,
                    },
                ))
                .unwrap()
                .result;
            let BoardResult::Inbox(inbox) = result else {
                panic!("expected inbox");
            };
            let event = inbox
                .events
                .iter()
                .find(|event| event.seq == decision.seq)
                .unwrap();
            assert_eq!(event.to, None);
            assert!(event.summary.as_str().len() <= 4096);
            assert!(event.summary.as_str().contains(&proposer.identity()));
            assert!(event.summary.as_str().contains(&proposal.entry.to_string()));
            assert!(event.summary.as_str().ends_with("..."));
        }
    }
}
