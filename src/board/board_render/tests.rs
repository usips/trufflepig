use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_ids::{BoardRef, EntryId, EventSeq, PlanId};
use crate::board::board_protocol::*;
use crate::board::board_vocabulary::{EntryKind, EntryText, ProposalState};
use crate::board::review_packet::{ReviewPacket, SsotDiff};

mod budget_render_tests;
mod inbox_render_tests;
mod review_render_tests;
mod view_render_tests;

fn actor() -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "c1",
    )
    .unwrap()
}

fn entry(seq: u64) -> EntryRecord {
    EntryRecord {
        via: None,
        id: EntryId::new(seq).unwrap(),
        plan: Some(PlanId::new(7).unwrap()),
        kind: EntryKind::Question,
        body: EntryText::new("still open even outside the recent window").unwrap(),
        to: None,
        supersedes: None,
        actor: actor(),
        model: None,
        effort: None,
        repo_key: None,
        state: None,
        refs: Vec::new(),
        seq: EventSeq::new(seq),
        created_at: 100,
    }
}

fn inbox(advancing: bool) -> InboxReply {
    let events = (11..23)
        .map(|seq| EventRecord {
            via: None,
            seq: EventSeq::new(seq),
            plan: Some(PlanId::new(7).unwrap()),
            kind: EntryKind::Progress,
            subject: BoardRef::Entry(EntryId::new(seq).unwrap()),
            to: None,
            actor: actor(),
            model: Some("gpt-6.1-sol".into()),
            effort: Some("xhigh".into()),
            summary: EntryText::new(format!(
                "event {seq} {}",
                "one fact with concrete evidence ".repeat(15)
            ))
            .unwrap(),
            created_at: 100,
        })
        .collect();
    InboxReply {
        actor: actor(),
        cursor: EventSeq::new(10),
        scanned_through: EventSeq::new(22),
        query_truncated: false,
        events,
        open: vec![entry(500)],
        open_omitted: 0,
        repo_key: None,
        all: true,
        latest: EventSeq::new(500),
        advancing,
        wait: InboxWait::None,
    }
}

fn review_packet() -> ReviewPacket {
    use crate::board::{board_ids::PlanRevision, board_vocabulary::PlanTitle};
    let plan = PlanId::new(7).unwrap();
    let base = PlanRevision::new(plan, 1).unwrap();
    let head = PlanRevision::new(plan, 2).unwrap();
    ReviewPacket {
        plan: PlanRecord {
            id: plan,
            title: PlanTitle::new("Review trial").unwrap(),
            owner_user: "josh".into(),
            steward: None,
            head_revision: 2,
            created_at: 100,
        },
        base,
        head,
        agent: None,
        ssot_diff: SsotDiff {
            before: base,
            after: head,
            hunks: Vec::new(),
            next: None,
        },
        entries: Vec::new(),
        tasks: Vec::new(),
        claims: Vec::new(),
        linked: Vec::new(),
        unlinked: Vec::new(),
        crossed: Vec::new(),
        open_proposals: Vec::new(),
        open_questions: Vec::new(),
        open_feedback: Vec::new(),
        scan_errors: Vec::new(),
        omitted: Default::default(),
    }
}

fn review_commit(number: u64) -> super::super::review_packet::ReviewCommit {
    use crate::board::board_ids::RepoKey;
    use crate::identity::GitOid;
    super::super::review_packet::ReviewCommit {
        commit: LinkedCommit {
            repo_key: RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap(),
            oid: GitOid::parse(&format!("{number:040x}")).unwrap(),
            subject: format!("Change {number} with concrete review evidence"),
            committed_at: number as i64,
            author: "Josh".into(),
            coauthors: Vec::new(),
            files: 1,
            insertions: 5,
            deletions: 2,
            plans: vec![CommitPlanLink {
                plan_id: PlanId::new(7).unwrap(),
                task_ordinal: Some(1),
            }],
        },
        drill: Some(format!(
            "trufflepig-agent --root '/source/review' diff {number:040x}"
        )),
    }
}
