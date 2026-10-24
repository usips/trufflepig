use super::*;
use crate::identity::GitOid;
use crate::{
    board::{
        board_actor::{AgentVendor, BoardRecipient},
        board_ids::TaskId,
        board_protocol::{
            AuthorityRole, AuthoritySource, ClaimEndReason, CommitClaimBasis, CommitClaimLink,
            CommitCoauthor, GateStatus, GrantRecord, GrantSource, LinkedCommit, PendingNotice,
            PendingNoticeKind, PendingNoticeReason, PendingReply, ProgressEvidence, ProgressFact,
            ProgressGateFact, ProgressHead, ProgressStep, ProgressTests, ReviewVerdict, TaskView,
            WorkerAddress, WorkerHandle, WorkerRecord, WorkerRole,
        },
        board_vocabulary::{EntryText, PlanTitle, TaskColumn},
    },
    request_context::ActorOrigin,
};

fn detailed_task() -> TaskView {
    let task = TaskId::new(PlanId::new(7).unwrap(), 3).unwrap();
    let claim_entry = EntryId::new(10).unwrap();
    let address = WorkerAddress::new(task, WorkerHandle::parse("reviewer").unwrap()).unwrap();
    let oid = || GitOid::parse(&"a".repeat(40)).unwrap();
    let progress = ProgressEvidence {
        claim: claim_entry,
        worker: Some(address.clone()),
        head: Some(ProgressFact {
            value: ProgressHead {
                oid: oid(),
                verified: true,
            },
            entry: EntryId::new(20).unwrap(),
            updated_at: 185,
        }),
        step: Some(ProgressFact {
            value: ProgressStep {
                current: 3,
                total: 5,
            },
            entry: EntryId::new(21).unwrap(),
            updated_at: 186,
        }),
        tests: Some(ProgressFact {
            value: ProgressTests {
                passed: 42,
                failed: 1,
            },
            entry: EntryId::new(22).unwrap(),
            updated_at: 187,
        }),
        gates: (0..32)
            .map(|index| ProgressGateFact {
                name: format!("gate-{index}-{}", "g".repeat(28)),
                status: GateStatus::Running,
                entry: EntryId::new(23).unwrap(),
                updated_at: 188,
            })
            .collect(),
        eta_at: Some(ProgressFact {
            value: 250,
            entry: EntryId::new(24).unwrap(),
            updated_at: 189,
        }),
    };
    let worker = WorkerRecord {
        address: address.clone(),
        claim: claim_entry,
        entry: EntryId::new(15).unwrap(),
        actor: Some(actor()),
        role: WorkerRole::Review,
        model: "gpt-6-luna".into(),
        effort: Some("xhigh".into()),
        scope: EntryText::new("review the task evidence").unwrap(),
        files: (0..32)
            .map(|index| format!("src/{}-{index}.rs", "long-path-component".repeat(8)))
            .collect(),
        state: None,
        added_at: 170,
        last_active: 190,
        ended_at: None,
        note: Some(EntryText::new("reviewing claim-linked commits").unwrap()),
        progress: Some(progress.clone()),
        verdict: Some(ReviewVerdict::Findings(2)),
    };
    let claim = ClaimRecord {
        task,
        actor: BoardActor::new(
            "system",
            "board",
            HarnessLabel::parse("system").unwrap(),
            "maintenance",
        )
        .unwrap(),
        vendor: AgentVendor::System,
        entry: claim_entry,
        scope: EntryText::new("maintain the renderer").unwrap(),
        claimed_at: 160,
        last_active: 190,
        ended_at: None,
        end_reason: None,
        stale: false,
        model: Some("system-maintenance".into()),
        effort: None,
        delegated_by: None,
        workers: vec![worker],
        workers_omitted: 0,
        progress: Some(progress),
        open_questions: 1,
        oldest_open_question: Some(EntryId::new(18).unwrap()),
        oldest_open_question_at: Some(175),
        commit_count: 1,
        latest_commit: Some(oid()),
        latest_commit_at: Some(198),
    };
    let mut history = claim.clone();
    history.ended_at = Some(150);
    history.end_reason = Some(ClaimEndReason::Released);
    history.workers.clear();
    history.progress = None;

    let mut newest = entry(20);
    newest.origin = Some(ActorOrigin {
        harness: HarnessLabel::parse("codex").unwrap(),
        session: "task-session".into(),
        peer_ip: Some("198.51.100.23".parse().unwrap()),
    });
    newest.authority = Some(AuthoritySource::Grant(EntryId::new(4).unwrap()));
    newest.verdict = Some(ReviewVerdict::Findings(2));
    let commit = LinkedCommit {
        repo_key: crate::board::board_ids::RepoKey::from_roots([oid()]).unwrap(),
        oid: oid(),
        subject: "Link task evidence".into(),
        committed_at: 198,
        author: "Josh".into(),
        coauthors: vec![CommitCoauthor {
            harness: HarnessLabel::parse("claude").unwrap(),
            model: "claude-sonnet".into(),
            email: "claude@example.test".into(),
        }],
        files: 2,
        insertions: 10,
        deletions: 3,
        plans: Vec::new(),
        worker_refs: vec![address],
        claim_links: vec![CommitClaimLink {
            task,
            claim: claim_entry,
            basis: CommitClaimBasis::ClaimWindow,
            actor: Some(actor()),
        }],
    };
    TaskView {
        task: TaskRecord {
            transferred_from: None,
            transferred_to: None,
            id: task,
            title: PlanTitle::new("Renderer task").unwrap(),
            column: TaskColumn::Doing,
            assignee: None,
            section: Some("Board".into()),
            seq: EventSeq::new(30),
            done_at: None,
        },
        current_claim: Some(claim),
        claim_history: vec![history],
        entries: vec![entry(18), entry(19), newest],
        commits: vec![commit],
        open_questions: 1,
        oldest_open_question: Some(EntryId::new(18).unwrap()),
        oldest_open_question_at: Some(175),
        latest_commit_at: Some(198),
        through: EventSeq::new(30),
        server_now: 200,
        claim_ttl_secs: 600,
        claims_omitted: 0,
        entries_omitted: 0,
        commits_omitted: 0,
    }
}

#[test]
fn task_budget_keeps_primary_records_and_trims_nested_worker_details_first() {
    let mut task = detailed_task();
    let task_id = task.task.id;
    let commit = task.commits.first_mut().unwrap();
    commit.worker_refs = (0..200)
        .map(|index| {
            WorkerAddress::new(
                task_id,
                WorkerHandle::parse(&format!("worker-{index}")).unwrap(),
            )
            .unwrap()
        })
        .collect();
    commit.claim_links = (0..200)
        .map(|index| CommitClaimLink {
            task: task_id,
            claim: EntryId::new(10 + index).unwrap(),
            basis: CommitClaimBasis::ClaimWindow,
            actor: Some(actor()),
        })
        .collect();
    let reply = BoardReply::new("local", BoardResult::TaskView(task));
    let budget = OutputBudget::new(800).unwrap();
    let rendered = render_reply(&reply, &budget).unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let visible: TaskView = serde_json::from_value(value["result"]["data"].clone()).unwrap();

    assert_eq!(value["result"]["result"], "task_view");
    assert_eq!(
        visible.task.id,
        TaskId::new(PlanId::new(7).unwrap(), 3).unwrap()
    );
    let current_claim = visible.current_claim.as_ref().unwrap();
    assert_eq!(current_claim.entry, EntryId::new(10).unwrap());
    assert_eq!(current_claim.workers.len(), 1);
    assert_eq!(
        visible.entries.first().unwrap().id,
        EntryId::new(20).unwrap()
    );
    assert!(
        value["omitted"]["worker_files"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    assert!(
        value["omitted"]["progress_gates"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    assert_eq!(value["omitted"]["workers"].as_u64().unwrap_or_default(), 0);
    let commits_omitted = value["omitted"]["commits"].as_u64().unwrap_or_default();
    let worker_refs_omitted = value["omitted"]["commit_worker_refs"]
        .as_u64()
        .unwrap_or_default();
    let claim_links_omitted = value["omitted"]["commit_claim_links"]
        .as_u64()
        .unwrap_or_default();
    assert!(commits_omitted + worker_refs_omitted + claim_links_omitted > 0);
    assert_eq!(visible.commits_omitted, commits_omitted as usize);
    if visible.commits.is_empty() {
        assert_eq!(commits_omitted, 1);
    } else {
        assert_eq!(visible.commits.len(), 1);
        assert_eq!(visible.commits[0].coauthors.len(), 1);
        assert!(visible.commits[0].worker_refs.len() < 200);
        assert!(visible.commits[0].claim_links.len() < 200);
        assert!(worker_refs_omitted + claim_links_omitted > 0);
    }
    assert!(budget.fits(&rendered.text));
}

#[test]
fn task_lines_show_progress_ages_sources_system_vendor_and_commit_attribution() {
    let reply = BoardReply::new("local", BoardResult::TaskView(detailed_task()));
    let budget = OutputBudget::new(5000)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = render_reply(&reply, &budget).unwrap();

    assert!(rendered.text.contains("vendor=system"));
    assert!(rendered.text.contains("origin=codex/task-session"));
    assert!(rendered.text.contains("peer_ip=198.51.100.23"));
    assert!(rendered.text.contains("authority=grant:E4"));
    assert!(rendered.text.contains("role=review model=gpt-6-luna"));
    assert!(rendered.text.contains("age=10s added_age=30s"));
    assert!(rendered.text.contains("end_reason=released"));
    assert!(rendered.text.contains("verdict=findings=2"));
    assert!(
        rendered
            .text
            .contains("tests=passed:42 failed:1 entry=E22 age=13s")
    );
    assert!(rendered.text.contains("eta_at=250 entry=E24 age=11s"));
    assert!(
        rendered
            .text
            .contains("coauthor harness=claude model=claude-sonnet")
    );
    assert!(
        rendered
            .text
            .contains("claim_link task=P7.3 claim=E10 basis=claim_window")
    );
    assert!(!rendered.text.contains("vendor=human"));
}

#[test]
fn inbox_event_lines_show_origin_authority_and_system_vendor() {
    let mut inbox = inbox(false);
    let event = inbox.events.first_mut().unwrap();
    event.origin = Some(ActorOrigin {
        harness: HarnessLabel::parse("muse").unwrap(),
        session: "event-session".into(),
        peer_ip: None,
    });
    event.authority = Some(AuthoritySource::Reporter);
    event.vendor = AgentVendor::System;
    let reply = BoardReply::new("local", BoardResult::Inbox(inbox));
    let budget = OutputBudget::new(5000)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = render_reply(&reply, &budget).unwrap();

    assert!(
        rendered
            .text
            .contains("vendor=system\torigin=muse/event-session\tauthority=reporter")
    );
    assert!(!rendered.text.contains("peer_ip="));
    assert!(!rendered.text.contains("vendor=human"));
}

#[test]
fn pending_budget_keeps_generation_and_counts_omitted_notices() {
    let notices = (11..19)
        .map(|seq| {
            let mut notice = entry(seq);
            notice.body = EntryText::new(format!(
                "notice {seq} {}",
                "question evidence with a useful bounded summary ".repeat(15)
            ))
            .unwrap();
            PendingNotice {
                seq: EventSeq::new(seq),
                kind: PendingNoticeKind::Entry,
                reason: PendingNoticeReason::Question,
                entry: Some(notice),
                event: None,
            }
        })
        .collect::<Vec<_>>();
    let reply = BoardReply::new(
        "local",
        BoardResult::Pending(PendingReply {
            notices,
            after: None,
            through: EventSeq::new(30),
            server_now: 200,
            generation: "generation-42".into(),
            omitted: 3,
        }),
    );
    let budget = OutputBudget::new(350).unwrap();
    let rendered = render_reply(&reply, &budget).unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let visible: PendingReply = serde_json::from_value(value["result"]["data"].clone()).unwrap();

    assert_eq!(visible.generation, "generation-42");
    assert!(!visible.notices.is_empty() && visible.notices.len() < 8);
    assert_eq!(visible.omitted, 3 + 8 - visible.notices.len());
    assert_eq!(value["omitted"]["notices"], 8 - visible.notices.len());
    assert!(value["next"].as_str().is_some());
    assert!(budget.fits(&rendered.text));
}

fn pending_question(seq: u64) -> PendingNotice {
    let mut record = entry(seq);
    record.body = EntryText::new(format!("open question {seq}")).unwrap();
    PendingNotice {
        seq: EventSeq::new(seq),
        kind: PendingNoticeKind::Entry,
        reason: PendingNoticeReason::Question,
        entry: Some(record),
        event: None,
    }
}

fn render_pending_json(
    notices: Vec<PendingNotice>,
    after: Option<u64>,
    omitted: usize,
    budget: &OutputBudget,
) -> Result<serde_json::Value, anyhow::Error> {
    let reply = BoardReply::new(
        "local",
        BoardResult::Pending(PendingReply {
            notices,
            after: after.map(EventSeq::new),
            through: EventSeq::new(30),
            server_now: 200,
            generation: "generation-42".into(),
            omitted,
        }),
    );
    let rendered = render_reply(&reply, budget)?;
    Ok(serde_json::from_str(&rendered.text).unwrap())
}

#[test]
fn pending_next_hint_resumes_after_new_notices_when_the_server_truncates() {
    let budget = OutputBudget::new(4000).unwrap();
    let truncated = render_pending_json(vec![pending_question(12)], Some(10), 4, &budget).unwrap();
    assert_eq!(truncated["next"], "board pending 12");
    let complete = render_pending_json(vec![pending_question(12)], Some(10), 0, &budget).unwrap();
    assert!(complete["next"].is_null());
}

#[test]
fn pending_next_hint_never_points_before_the_request_cursor() {
    // New notice 25 sorts first; open reminders 10 and 12 follow it.
    let notices = || {
        vec![
            pending_question(25),
            pending_question(10),
            pending_question(12),
        ]
    };
    let mut visible_counts = Vec::new();
    for limit in (60..=800).step_by(5) {
        let budget = OutputBudget::new(limit).unwrap();
        let Ok(value) = render_pending_json(notices(), Some(20), 0, &budget) else {
            continue;
        };
        let visible = value["result"]["data"]["notices"].as_array().unwrap().len();
        visible_counts.push(visible);
        match visible {
            // Only the new notice is past the cursor, so only it can be resumed from.
            1 | 2 => assert_eq!(value["next"], "board pending 25", "budget {limit}"),
            _ => assert!(value["next"].is_null(), "budget {limit}: {value}"),
        }
    }
    assert!(
        visible_counts.contains(&2),
        "the budget sweep must cut between the new and reminder notices: {visible_counts:?}"
    );
}

#[test]
fn pending_lines_show_a_notice_without_one_payload_as_invalid() {
    let notice = PendingNotice {
        seq: EventSeq::new(12),
        kind: PendingNoticeKind::Entry,
        reason: PendingNoticeReason::Mention,
        entry: None,
        event: None,
    };
    let reply = BoardReply::new(
        "local",
        BoardResult::Pending(PendingReply {
            notices: vec![notice],
            after: None,
            through: EventSeq::new(30),
            server_now: 200,
            generation: "generation-42".into(),
            omitted: 0,
        }),
    );
    let budget = OutputBudget::new(5000)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = render_reply(&reply, &budget).unwrap();
    assert!(rendered.text.contains("12\tkind=invalid reason=mention"));
}

#[test]
fn plan_lines_render_grant_rows_and_manage_permission() {
    let plan_id = PlanId::new(7).unwrap();
    let plan = PlanRecord {
        lifecycle_state: crate::board::board_protocol::PlanLifecycleState::Open,
        id: plan_id,
        title: PlanTitle::new("Grant visibility").unwrap(),
        owner_user: "josh".into(),
        head_revision: 1,
        created_at: 100,
    };
    let plan_view = PlanView {
        can_change_lifecycle: false,
        repo_keys: Vec::new(),
        plan,
        revision: RevisionRecord {
            id: crate::board::board_ids::PlanRevision::new(plan_id, 1).unwrap(),
            body: crate::board::board_vocabulary::PlanText::new("# Grant visibility\n").unwrap(),
            source: RevisionSource::Create,
            entry: EntryId::new(1).unwrap(),
            actor: actor(),
            seq: EventSeq::new(1),
            created_at: 100,
        },
        tasks: Vec::new(),
        grants: vec![GrantRecord {
            entry: EntryId::new(9).unwrap(),
            plan: Some(plan_id),
            owner_user: "josh".into(),
            roles: vec![AuthorityRole::Triage, AuthorityRole::Assign],
            grantee: BoardRecipient::parse("claude/reviewer").unwrap(),
            granted_at: 110,
            expires_at: Some(200),
            revoked_by: None,
            source: GrantSource::Owner,
        }],
        can_manage_grants: true,
        task_ceiling: crate::board::board_collections::TaskCeiling {
            plan: plan_id,
            ordinal: 0,
        },
        claims: Vec::new(),
        entries: Vec::new(),
        commits: Vec::new(),
        tasks_omitted: 0,
        claims_omitted: 0,
        entries_omitted: 0,
        commits_omitted: 0,
        tasks_next_after: None,
        claims_next_after: None,
        entries_next_before: None,
        through: EventSeq::new(1),
        can_edit: true,
        server_now: 120,
        claim_ttl_secs: 600,
        sections_without_tasks: Vec::new(),
    };
    let reply = BoardReply::new("local", BoardResult::Plan(plan_view));
    let budget = OutputBudget::new(2000)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = render_reply(&reply, &budget).unwrap();

    assert!(
        rendered
            .text
            .contains("permissions: edit=true manage_grants=true")
    );
    assert!(
        rendered
            .text
            .contains("grant E9 plan=P7 owner=josh grantee=claude/reviewer")
    );
    assert!(rendered.text.contains("roles=triage,assign"));
    assert!(!rendered.text.contains("steward"));
}
