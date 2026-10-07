mod review_diff;
mod warning_suppression;

use super::*;
use crate::board::{
    board_actor::BoardActor,
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

fn task_record(plan: PlanId, ordinal: u64) -> TaskRecord {
    TaskRecord {
        id: TaskId::new(plan, ordinal).unwrap(),
        title: PlanTitle::new(format!("Review task {ordinal}")).unwrap(),
        column: TaskColumn::Doing,
        assignee: None,
        section: None,
        seq: EventSeq::new(ordinal),
    }
}

fn manual_link(
    commit: &LinkedCommit,
    plan: PlanId,
    task_ordinal: u64,
    entry: u64,
    seq: Option<u64>,
    actor: Option<BoardActor>,
) -> ManualCommitLink {
    ManualCommitLink {
        repo_key: commit.repo_key.clone(),
        oid: commit.oid,
        task: TaskId::new(plan, task_ordinal).unwrap(),
        entry: EntryId::new(entry).unwrap(),
        seq: seq.map(EventSeq::new),
        actor,
    }
}

#[test]
fn review_names_the_hand_linker_when_only_a_scan_entry_exists() {
    let mut source = evidence();
    let scanned = commit("codex", 120);
    source.tasks.push(task_record(source.plan.id, 1));
    let entry = commit_entry(&scanned, &format!("git-{}", scanned.oid));
    source.manual_links.push(manual_link(
        &scanned,
        source.plan.id,
        1,
        entry.id.get(),
        Some(91),
        Some(actor_with_session("human", "linker")),
    ));
    source.entries.push(entry);
    source.commits.push(scanned);
    let packet = assemble_review(
        &source,
        Some(&HarnessLabel::parse("codex").unwrap()),
        &[],
        Vec::new(),
        Vec::new(),
    );
    assert!(
        packet.entries.is_empty(),
        "the human entry is agent-filtered"
    );
    assert_eq!(
        packet.linked[0].manual_links[0]
            .actor
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
            .contains("task P7.1 linked by hand by josh@laptop/human/linker (entry E90, seq 91)"),
        "{}",
        rendered.text
    );
}

#[test]
fn review_attaches_all_manual_links_for_the_exact_commit_and_plan() {
    let mut source = evidence();
    let linked = commit("codex", 120);
    source.tasks = [1, 2]
        .into_iter()
        .map(|ordinal| task_record(source.plan.id, ordinal))
        .collect();
    let other_plan = PlanId::new(8).unwrap();
    let mut other_repo = linked.clone();
    other_repo.repo_key = RepoKey::from_roots([GitOid::parse(&"c".repeat(40)).unwrap()]).unwrap();
    let mut other_oid = linked.clone();
    other_oid.oid = GitOid::parse(&"d".repeat(40)).unwrap();
    source.manual_links = vec![
        manual_link(
            &linked,
            source.plan.id,
            2,
            92,
            Some(92),
            Some(actor("claude")),
        ),
        manual_link(
            &other_repo,
            source.plan.id,
            1,
            93,
            Some(93),
            Some(actor("grok")),
        ),
        manual_link(&linked, other_plan, 1, 94, Some(94), Some(actor("muse"))),
        manual_link(
            &other_oid,
            source.plan.id,
            1,
            95,
            Some(95),
            Some(actor("omp")),
        ),
        manual_link(&linked, source.plan.id, 1, 91, None, None),
    ];
    source.commits.push(linked);

    let packet = assemble_review(&source, None, &[], Vec::new(), Vec::new());
    let links = &packet.linked[0].manual_links;
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].task.ordinal, 1);
    assert!(links[0].actor.is_none());
    assert_eq!(links[1].task.ordinal, 2);
    assert_eq!(links[1].actor.as_ref().unwrap().harness.as_str(), "claude");

    let budget = OutputBudget::new(32768)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = render_review(&packet, &budget, "local").unwrap();
    assert!(rendered.text.contains(
        "task P7.1 linked by hand; historical attribution unknown (entry E91, seq unknown)"
    ));
    assert!(
        rendered
            .text
            .contains("task P7.2 linked by hand by josh@laptop/claude/c1 (entry E92, seq 92)")
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
fn assembly_preserves_unscoped_commit_warnings() {
    let mut source = evidence();
    let linked = commit("codex", 120);
    source.commits.push(linked.clone());
    let warning = format!("board_scan: {}: misplaced_trailers", linked.oid);
    let packet = assemble_review(&source, None, &[], Vec::new(), vec![warning.clone()]);
    assert_eq!(packet.scan_errors, vec![warning]);
}

#[test]
fn scan_commit_entries_are_not_hand_links() {
    let mut source = evidence();
    let scanned = commit("codex", 120);
    let session = format!("git-{}", scanned.oid);
    source.entries.push(commit_entry(&scanned, &session));
    source.commits.push(scanned);
    let packet = assemble_review(&source, None, &[], Vec::new(), Vec::new());
    assert!(packet.linked[0].manual_links.is_empty());
}
