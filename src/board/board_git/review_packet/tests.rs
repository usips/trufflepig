mod review_claim_lane_tests;
mod review_manual_link_tests;

mod review_diff;
mod warning_suppression;

use super::*;
use crate::board::{
    board_actor::{AgentVendor, BoardActor, claim_vendor},
    board_ids::{EntryId, EventSeq, PlanId, RepoKey, TaskId},
    board_protocol::{
        CommitCoauthor, CommitPlanLink, ManualCommitLink, RevisionRecord, RevisionSource,
    },
    board_render::render_review,
    board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle, TaskColumn},
};
use crate::{
    identity::GitOid,
    output::{OutputBudget, OutputFormat},
};

fn actor(harness: &str) -> BoardActor {
    actor_with_session(harness, "c1")
}

fn actor_with_session(harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn revision(number: u64, body: &str) -> RevisionRecord {
    RevisionRecord {
        id: PlanRevision::new(PlanId::new(7).unwrap(), number).unwrap(),
        body: PlanText::new(body).unwrap(),
        source: RevisionSource::Create,
        entry: EntryId::new(number).unwrap(),
        actor: actor("human"),
        seq: EventSeq::new(number),
        created_at: 100,
    }
}

fn evidence() -> ReviewEvidence {
    let plan = PlanId::new(7).unwrap();
    ReviewEvidence {
        plan: PlanRecord {
            id: plan,
            title: PlanTitle::new("Trial").unwrap(),
            owner_user: "josh".into(),
            steward: None,
            head_revision: 2,
            created_at: 100,
        },
        agent: Some(HarnessLabel::parse("codex").unwrap()),
        window_end: 200,
        base: revision(1, "same\nold\ncontext\n"),
        head: revision(2, "same\nnew\ncontext\n"),
        entries: Vec::new(),
        tasks: Vec::new(),
        claims: Vec::new(),
        commits: Vec::new(),
        manual_links: Vec::new(),
        open_proposals: Vec::new(),
        open_questions: Vec::new(),
        open_feedback: Vec::new(),
    }
}

fn commit(harness: &str, time: i64) -> LinkedCommit {
    LinkedCommit {
        repo_key: RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap(),
        oid: GitOid::parse(&"b".repeat(40)).unwrap(),
        subject: "Implement parser".into(),
        committed_at: time,
        author: "Josh".into(),
        coauthors: vec![CommitCoauthor {
            harness: HarnessLabel::parse(harness).unwrap(),
            model: "claimed model".into(),
            email: "agent@example.com".into(),
        }],
        files: 1,
        insertions: 5,
        deletions: 2,
        plans: vec![CommitPlanLink {
            plan_id: PlanId::new(7).unwrap(),
            task_ordinal: Some(1),
        }],
    }
}
