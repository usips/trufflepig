use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn stale_proposal_reminders_reach_new_sessions_of_the_same_harness() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let base = PlanRevision::new(plan, 1).unwrap();
    let BoardResult::Change(stale) = call(
        &mut board,
        "codex",
        BoardOp::Propose {
            supersedes: None,
            base,
            body: PlanText::new("# Stale").unwrap(),
            summary: EntryText::new("stale proposal reminder").unwrap(),
        },
    ) else {
        panic!("missing stale proposal")
    };
    let BoardResult::Change(advance) = call(
        &mut board,
        "muse",
        BoardOp::Propose {
            supersedes: None,
            base,
            body: PlanText::new("# Advanced").unwrap(),
            summary: EntryText::new("advance the head").unwrap(),
        },
    ) else {
        panic!("missing head advance")
    };
    call(
        &mut board,
        "human",
        BoardOp::Accept {
            proposal: advance.entry,
            note: None,
        },
    );
    let reminders = |board: &mut LocalBoard, harness: &str, session: &str| {
        let actor = BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse(harness).unwrap(),
            session,
        )
        .unwrap();
        match board
            .handle(&BoardRequest::new(
                actor,
                BoardOp::Inbox {
                    scope: ReadScope::All,
                    after: Some(EventSeq::new(0)),
                    limit: 100,
                },
            ))
            .unwrap()
            .result
        {
            BoardResult::Inbox(inbox) => inbox.open,
            other => panic!("unexpected {other:?}"),
        }
    };
    let second = reminders(&mut board, "codex", "session2");
    assert!(
        second.iter().any(|entry| entry.id == stale.entry),
        "a new session of the proposing harness keeps the rebase reminder: {second:?}"
    );
    let foreign = reminders(&mut board, "muse", "session2");
    assert!(
        !foreign.iter().any(|entry| entry.id == stale.entry),
        "another harness still omits the stale proposal"
    );
}
