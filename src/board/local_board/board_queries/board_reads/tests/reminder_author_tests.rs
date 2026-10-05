use super::*;

#[test]
fn stale_proposal_reminders_stay_visible_only_to_their_author() {
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
    let reminders = |board: &mut LocalBoard, user: &str, harness: &str| {
        let actor = BoardActor::new(
            user,
            "laptop",
            HarnessLabel::parse(harness).unwrap(),
            "session1",
        )
        .unwrap();
        match board
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
        {
            BoardResult::Inbox(inbox) => inbox.open,
            other => panic!("unexpected {other:?}"),
        }
    };
    let author = reminders(&mut board, "josh", "codex");
    assert!(
        author.iter().any(|entry| entry.id == stale.entry),
        "author reminders keep the stale proposal: {author:?}"
    );
    for (user, harness) in [("josh", "muse"), ("other", "codex")] {
        let open = reminders(&mut board, user, harness);
        assert!(
            !open.iter().any(|entry| entry.id == stale.entry),
            "{user}/{harness} reminders omit another author's stale proposal"
        );
    }
    let BoardResult::Change(current) = call(
        &mut board,
        "codex",
        BoardOp::Propose {
            supersedes: None,
            base: PlanRevision::new(plan, 2).unwrap(),
            body: PlanText::new("# Current").unwrap(),
            summary: EntryText::new("current proposal reminder").unwrap(),
        },
    ) else {
        panic!("missing current proposal")
    };
    let other = reminders(&mut board, "josh", "muse");
    assert!(
        other.iter().any(|entry| entry.id == current.entry),
        "current proposals keep their existing visibility"
    );
}
