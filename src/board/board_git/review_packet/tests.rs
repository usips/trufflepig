mod review_diff;

use super::*;
use crate::board::{
    board_actor::BoardActor,
    board_ids::{EntryId, EventSeq, PlanId, RepoKey, TaskId},
    board_protocol::{CommitCoauthor, CommitPlanLink, RevisionRecord, RevisionSource},
    board_render::render_review,
    board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle},
};
use crate::{
    identity::GitOid,
    output::{OutputBudget, OutputFormat},
};

fn actor(harness: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        "c1",
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

#[test]
fn review_uses_claim_intervals_and_coauthors_to_flag_crossed_lanes() {
    let mut source = evidence();
    source.claims.push(ClaimRecord {
        task: TaskId::new(source.plan.id, 1).unwrap(),
        actor: actor("claude"),
        entry: EntryId::new(10).unwrap(),
        scope: EntryText::new("parser only").unwrap(),
        claimed_at: 110,
        last_active: 120,
        ended_at: Some(150),
        end_reason: Some(crate::board::board_protocol::ClaimEndReason::Released),
        stale: false,
        model: Some("opus".into()),
        effort: None,
        delegated_by: None,
    });
    source.commits = vec![
        commit("codex", 120),
        commit("codex", 150),
        commit("claude", 130),
    ];
    let packet = assemble_review(&source, source.agent.as_ref(), &[], Vec::new(), Vec::new());
    assert_eq!(packet.crossed.len(), 1);
    assert_eq!(packet.crossed[0].claimant.harness.as_str(), "claude");
    assert!(packet.claims.is_empty(), "reviewed agent owns no claims");
    assert_eq!(
        packet.linked.len(),
        3,
        "all plan-linked commits remain reviewable"
    );
}

#[test]
fn review_uses_claim_model_vendor_for_muse_omp_and_cli() {
    for (harness, model, vendor) in [
        ("muse", Some("claude-opus"), "claude"),
        ("omp", Some("gpt-6.1"), "codex"),
        ("cli", None, "human"),
    ] {
        let mut source = evidence();
        source.claims.push(ClaimRecord {
            task: TaskId::new(source.plan.id, 1).unwrap(),
            actor: actor(harness),
            entry: EntryId::new(10).unwrap(),
            scope: EntryText::new("same vendor").unwrap(),
            claimed_at: 110,
            last_active: 120,
            ended_at: None,
            end_reason: None,
            stale: false,
            model: model.map(str::to_owned),
            effort: None,
            delegated_by: None,
        });
        let mut same = commit(vendor, 120);
        if vendor == "human" {
            same.coauthors.clear();
        }
        source.commits.push(same);
        let packet = assemble_review(&source, None, &[], Vec::new(), Vec::new());
        assert!(
            packet.crossed.is_empty(),
            "same vendor claim crossed for {harness}"
        );
        source.commits = vec![commit("grok", 120)];
        let packet = assemble_review(&source, None, &[], Vec::new(), Vec::new());
        assert_eq!(
            packet.crossed.len(),
            1,
            "different vendor claim not crossed for {harness}"
        );
    }
}

#[test]
fn unlinked_commits_share_the_backend_snapshot_window() {
    let source = evidence();
    let unlinked = [99, 100, 200, 201]
        .into_iter()
        .map(|time| {
            let mut record = commit("codex", time);
            record.oid = GitOid::parse(&format!("{time:040x}")).unwrap();
            record
        })
        .collect();
    let packet = assemble_review(&source, source.agent.as_ref(), &[], unlinked, Vec::new());
    assert_eq!(
        packet
            .unlinked
            .iter()
            .map(|item| item.commit.committed_at)
            .collect::<Vec<_>>(),
        [100, 200]
    );
}

#[test]
fn duplicate_clones_do_not_duplicate_unlinked_commit_evidence() {
    let source = evidence();
    let one = commit("codex", 120);
    let mut other_repo = one.clone();
    other_repo.repo_key = RepoKey::from_roots([GitOid::parse(&"c".repeat(40)).unwrap()]).unwrap();
    let packet = assemble_review(
        &source,
        source.agent.as_ref(),
        &[],
        vec![one.clone(), one, other_repo],
        Vec::new(),
    );
    assert_eq!(packet.unlinked.len(), 2, "identity is repository plus oid");
}

fn commit_entry(commit: &LinkedCommit, session: &str) -> EntryRecord {
    EntryRecord {
        id: EntryId::new(90).unwrap(),
        via: None,
        plan: Some(PlanId::new(7).unwrap()),
        kind: EntryKind::Commit,
        body: EntryText::new(format!("{} {}", commit.oid, commit.subject)).unwrap(),
        to: None,
        supersedes: None,
        actor: BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse("human").unwrap(),
            session,
        )
        .unwrap(),
        model: None,
        effort: None,
        repo_key: Some(commit.repo_key.clone()),
        state: None,
        refs: Vec::new(),
        seq: EventSeq::new(90),
        created_at: 150,
    }
}

#[test]
fn review_names_the_hand_linker_and_renders_it() {
    let mut source = evidence();
    let manual = commit("human", 120);
    source.entries.push(commit_entry(&manual, "linker"));
    source.commits.push(manual);
    let packet = assemble_review(&source, None, &[], Vec::new(), Vec::new());
    assert_eq!(
        packet.linked[0]
            .linked_by
            .as_ref()
            .map(BoardActor::identity)
            .as_deref(),
        Some("josh@laptop/human/linker")
    );
    let budget = OutputBudget::new(32768)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = render_review(&packet, &budget, "local").unwrap();
    assert!(
        rendered
            .text
            .contains("linked by hand by josh@laptop/human/linker"),
        "{}",
        rendered.text
    );
}

#[test]
fn manually_linked_commit_leaves_unlinked_list() {
    let mut source = evidence();
    let manual = commit("codex", 120);
    source.entries.push(commit_entry(&manual, "linker"));
    source.commits.push(manual.clone());
    let packet = assemble_review(
        &source,
        source.agent.as_ref(),
        &[],
        vec![manual],
        Vec::new(),
    );
    assert_eq!(packet.linked.len(), 1);
    assert!(
        packet.unlinked.is_empty(),
        "a linked commit is never also unlinked: {:?}",
        packet
            .unlinked
            .iter()
            .map(|item| item.commit.oid)
            .collect::<Vec<_>>()
    );
}

#[test]
fn linked_commit_warnings_leave_review_scan_errors() {
    let mut source = evidence();
    let linked = commit("codex", 120);
    source.commits.push(linked.clone());
    let warning = format!("board_scan: {}: misplaced_trailers", linked.oid);
    let packet = assemble_review(&source, None, &[], Vec::new(), vec![warning]);
    assert!(
        packet.scan_errors.is_empty(),
        "warnings for linked commits are noise: {:?}",
        packet.scan_errors
    );
}

#[test]
fn scan_commit_entries_are_not_hand_links() {
    let mut source = evidence();
    let scanned = commit("codex", 120);
    let session = format!("git-{}", scanned.oid);
    source.entries.push(commit_entry(&scanned, &session));
    source.commits.push(scanned);
    let packet = assemble_review(&source, None, &[], Vec::new(), Vec::new());
    assert!(packet.linked[0].linked_by.is_none());
}
