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
        end_reason: Some(super::super::board_protocol::ClaimEndReason::Released),
        stale: false,
        model: Some("opus".into()),
        effort: None,
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
fn ssot_diff_retains_original_newline_and_crlf_bytes() {
    let diff = build_ssot_diff(&revision(1, "same\r\nold\r\n"), &revision(2, "same\r\nnew"));
    assert_eq!(diff.hunks.len(), 1);
    assert_eq!(diff.hunks[0].removed, ["old\r\n"]);
    assert_eq!(diff.hunks[0].added, ["new"]);
    assert_eq!(diff.hunks[0].context_before, ["same\r\n"]);
}

#[test]
fn adjacent_diff_context_contains_only_unchanged_lines() {
    let diff = build_ssot_diff(
        &revision(1, "start\nold a\nmiddle\nold b\nend\n"),
        &revision(2, "start\nnew a\nmiddle\nnew b\nend\n"),
    );
    assert_eq!(diff.hunks.len(), 2);
    for hunk in &diff.hunks {
        for line in hunk.context_before.iter().chain(&hunk.context_after) {
            assert!(matches!(line.as_str(), "start\n" | "middle\n" | "end\n"));
        }
    }
}

#[test]
fn insertion_diff_header_counts_the_context_it_displays() {
    let reply = super::super::board_protocol::BoardReply::new(
        "local",
        super::super::board_protocol::BoardResult::Diff(
            super::super::board_protocol::RevisionDiff {
                before: revision(1, "A\nB\n"),
                after: revision(2, "A\nX\nB\n"),
            },
        ),
    );
    let budget = OutputBudget::new(500)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = super::super::board_render::render_reply(&reply, &budget).unwrap();
    assert!(rendered.text.contains("@@ -1,2 +1,3 @@\n A\n+X\n B\n"));
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

#[test]
fn review_trimming_reports_each_omission_and_keeps_drill_and_diff_hint() {
    let mut source = evidence();
    for seq in 10..20 {
        source.entries.push(EntryRecord {
            id: EntryId::new(seq).unwrap(),
            plan: Some(source.plan.id),
            kind: EntryKind::Progress,
            body: EntryText::new("entry with individually useful facts ".repeat(90)).unwrap(),
            to: None,
            supersedes: None,
            actor: actor("codex"),
            model: None,
            effort: None,
            repo_key: None,
            state: None,
            refs: Vec::new(),
            seq: EventSeq::new(seq),
            created_at: 120,
        });
    }
    source.base = revision(
        1,
        &format!("context\n{}", "old text for line\n".repeat(1000)),
    );
    source.head = revision(
        2,
        &format!("context\n{}", "new text for line\n".repeat(1000)),
    );
    let linked = commit("codex", 120);
    source.commits.push(linked.clone());
    let repositories = vec![RepoScanTarget {
        registration: super::super::board_protocol::RepoRegistration {
            repo_key: linked.repo_key.clone(),
            origin_label: None,
            host: "laptop".into(),
            common_dir: "/source/it's here/.git".into(),
            plan_id: Some(source.plan.id),
        },
        oldest_plan_at: 100,
        plans: vec![source.plan.id],
        scan_error: None,
    }];
    let packet = assemble_review(
        &source,
        source.agent.as_ref(),
        &repositories,
        Vec::new(),
        Vec::new(),
    );
    assert!(packet.linked[0].drill.as_ref().unwrap().contains("'\"'\"'"));
    let budget = OutputBudget::new(900).unwrap();
    let rendered = render_review(&packet, &budget, "local").unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let data = &value["result"]["data"];
    assert_eq!(data["omitted"]["entries"], 10);
    assert!(data["omitted"]["diff_context_lines"].as_u64().unwrap() > 0);
    assert_eq!(data["omitted"]["diff_body_lines"], 2000);
    assert_eq!(data["ssot_diff"]["next"], "board show P7@1..2");
    assert!(
        data["linked"][0]["drill"]
            .as_str()
            .unwrap()
            .contains("trufflepig --root")
    );
    assert!(budget.fits(&rendered.text));
    let lines = render_review(
        &packet,
        &OutputBudget::new(900)
            .unwrap()
            .with_format(OutputFormat::Lines),
        "local",
    )
    .unwrap();
    assert!(lines.text.contains("omitted: entries=10"));
    assert!(lines.text.contains("next: board show P7@1..2"));
    assert!(lines.text.contains("commit trailer: Plan: P7"));
    assert!(
        lines
            .text
            .contains("coauthor: codex (claimed model) <agent@example.com>")
    );
    assert!(lines.text.contains("Plan: P7 Plan-Task: P7.1"));
}
