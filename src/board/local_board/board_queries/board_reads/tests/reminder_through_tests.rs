use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn proposal_reminders_read_currency_as_of_through() {
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
    let foreign = WriteContext {
        actor_id: -1,
        actor: BoardActor::new(
            "other",
            "laptop",
            HarnessLabel::parse("codex").unwrap(),
            "s1",
        )
        .unwrap(),
        model: None,
        effort: None,
        now: 100,
        seq: EventSeq::new(0),
        claim_ttl_secs: 20,
        via: None,
    };
    let visible = |through: u64| {
        open_entries(
            &board.conn,
            &foreign,
            &ReadScope::All,
            20,
            EventSeq::new(through),
        )
        .unwrap()
        .0
        .iter()
        .any(|entry| entry.id == stale.entry)
    };
    assert!(
        visible(3),
        "the proposal was current before the head advanced"
    );
    assert!(
        !visible(board.max_seq().unwrap().get()),
        "the proposal is stale at the live head for a foreign reader"
    );
}
