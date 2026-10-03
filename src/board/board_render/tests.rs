use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_ids::{BoardRef, EntryId};
use crate::board::board_vocabulary::EntryText;
use crate::board::review_packet::ReviewPacket;

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
        events,
        open: vec![entry(500)],
        latest: EventSeq::new(500),
        advancing,
        wait: InboxWait::None,
    }
}

#[test]
fn budget_truncated_inbox_acknowledges_only_the_visible_prefix() {
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(800).unwrap().with_format(format);
        let reply = BoardReply::new("local:/board", BoardResult::Inbox(inbox(true)));
        let rendered = render_reply(&reply, &budget).unwrap();
        let last = rendered.rendered_seq.unwrap().get();
        assert!((11..22).contains(&last));
        assert!(budget.fits(&rendered.text));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            let events = value["result"]["data"]["events"].as_array().unwrap();
            assert_eq!(events.last().unwrap()["seq"], last);
            assert_eq!(value["omitted"]["events"], 22 - last);
            assert_eq!(value["rendered_through"], last);
            assert_eq!(value["result"]["data"]["open"][0]["seq"], 500);
        } else {
            assert!(rendered.text.contains(&format!("next: board inbox {last}")));
            assert!(rendered.text.contains("--- still open ---"));
            assert!(!rendered.text.contains(&format!("{}\tP7", last + 1)));
        }
    }
}

#[test]
fn explicit_inbox_reads_and_open_only_reminders_do_not_acknowledge() {
    let budget = OutputBudget::new(800).unwrap();
    let reread = BoardReply::new("local", BoardResult::Inbox(inbox(false)));
    assert!(
        render_reply(&reread, &budget)
            .unwrap()
            .rendered_seq
            .is_none()
    );
    let mut reminders = inbox(true);
    reminders.events.clear();
    let reply = BoardReply::new("local", BoardResult::Inbox(reminders));
    assert!(
        render_reply(&reply, &budget)
            .unwrap()
            .rendered_seq
            .is_none()
    );
}

#[test]
fn oversized_first_event_fails_without_skipping_to_smaller_events() {
    let mut feed = inbox(true);
    feed.events[0].summary = EntryText::new("many individual words ".repeat(180)).unwrap();
    for event in feed.events.iter_mut().skip(1) {
        event.summary = EntryText::new("small").unwrap();
    }
    let reply = BoardReply::new("local", BoardResult::Inbox(feed));
    let error = render_reply(&reply, &OutputBudget::new(150).unwrap()).unwrap_err();
    assert!(error.to_string().starts_with("budget_too_small:"));
}

#[test]
fn warnings_survive_list_fitting_and_lines_cannot_inject_metadata() {
    let mut reply = BoardReply::new(
        "local\ncommit trailer: fake",
        BoardResult::Plans(Vec::new()),
    );
    reply.warnings.push("scan failed\nnext: forged".into());
    let json = render_reply(&reply, &OutputBudget::new(500).unwrap()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json.text).unwrap();
    assert_eq!(value["warnings"][0], reply.warnings[0]);
    let lines = render_reply(
        &reply,
        &OutputBudget::new(500)
            .unwrap()
            .with_format(OutputFormat::Lines),
    )
    .unwrap();
    assert!(lines.text.contains("warning: scan failed\\nnext: forged\n"));
    assert!(!lines.text.contains("\ncommit trailer: fake\n"));
}

#[test]
fn fit_items_binary_search_counts_complete_responses() {
    let budget = OutputBudget::new(30).unwrap();
    let render = |count| {
        Ok(format!(
            "fixed metadata\n{}omitted={}\nbackend local\n",
            "item\n".repeat(count),
            20 - count
        ))
    };
    let count = fit_items(20, &budget, render).unwrap();
    assert!(budget.fits(&render(count).unwrap()));
    assert!(!budget.fits(&render(count + 1).unwrap()));
}

#[test]
fn open_evidence_truncation_is_disclosed_without_driving_the_cursor() {
    let mut feed = inbox(true);
    feed.events.truncate(1);
    feed.events[0].summary = EntryText::new("fresh fact").unwrap();
    feed.open = (500..510)
        .map(|seq| {
            let mut reminder = entry(seq);
            reminder.body = EntryText::new("long open question evidence ".repeat(100)).unwrap();
            reminder
        })
        .collect();
    let rendered = render_reply(
        &BoardReply::new("local", BoardResult::Inbox(feed)),
        &OutputBudget::new(800).unwrap(),
    )
    .unwrap();
    assert_eq!(rendered.rendered_seq, Some(EventSeq::new(11)));
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let shown = value["result"]["data"]["open"].as_array().unwrap().len();
    assert_eq!(value["omitted"]["open_entries"], 10 - shown);
    assert!(shown < 10);
    assert!(
        value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("board show P7"))
    );
}

#[test]
fn show_preserves_labor_and_uncovered_sections_while_trimming_the_body() {
    use crate::board::{
        board_ids::{PlanRevision, TaskId},
        board_vocabulary::{PlanText, PlanTitle, TaskColumn},
    };
    let plan = PlanId::new(7).unwrap();
    let task = |ordinal, column| TaskRecord {
        id: TaskId::new(plan, ordinal).unwrap(),
        title: PlanTitle::new(format!("task {ordinal}")).unwrap(),
        column,
        assignee: None,
        section: Some("Parser".into()),
        seq: EventSeq::new(10),
    };
    let claim = |ordinal, harness: &str, stale, ended_at, scope: &str| ClaimRecord {
        task: TaskId::new(plan, ordinal).unwrap(),
        actor: BoardActor::new("josh", "laptop", HarnessLabel::parse(harness).unwrap(), "s")
            .unwrap(),
        entry: EntryId::new(ordinal).unwrap(),
        scope: EntryText::new(scope).unwrap(),
        claimed_at: 100,
        last_active: 120,
        ended_at,
        end_reason: ended_at.map(|_| ClaimEndReason::Released),
        stale,
        model: Some("claimed-model".into()),
        effort: Some("xhigh".into()),
    };
    let mut recent = entry(100);
    recent.kind = EntryKind::Progress;
    recent.body = EntryText::new("recent long evidence ".repeat(150)).unwrap();
    let view = PlanView {
        plan: PlanRecord {
            id: plan,
            title: PlanTitle::new("Trial").unwrap(),
            owner_user: "josh".into(),
            steward: None,
            head_revision: 1,
            created_at: 100,
        },
        revision: RevisionRecord {
            id: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("long SSOT section with facts\n".repeat(1000)).unwrap(),
            source: RevisionSource::Create,
            entry: EntryId::new(1).unwrap(),
            actor: actor(),
            seq: EventSeq::new(1),
            created_at: 100,
        },
        tasks: vec![
            task(1, TaskColumn::Doing),
            task(2, TaskColumn::Doing),
            task(3, TaskColumn::Todo),
            task(4, TaskColumn::Review),
        ],
        claims: vec![
            claim(1, "codex", false, None, "parser only"),
            claim(2, "muse", true, None, "stale work"),
            claim(
                4,
                "claude",
                false,
                Some(130),
                "released scope must disappear",
            ),
        ],
        entries: vec![recent],
        sections_without_tasks: vec!["Uncovered heading".into()],
    };
    let budget = OutputBudget::new(800)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let text = render_reply(&BoardReply::new("local", BoardResult::Plan(view)), &budget)
        .unwrap()
        .text;
    let headings = [
        "--- working now ---",
        "--- open for claiming ---",
        "--- plan sections without tasks ---",
        "--- P7@1 Trial ---",
        "--- recent evidence ---",
        "backend:",
    ];
    let positions = headings.map(|heading| text.find(heading).unwrap());
    assert!(
        positions
            .windows(2)
            .all(|positions| positions[0] < positions[1])
    );
    assert!(text.contains("parser only"));
    assert!(text.contains("claimed-model/xhigh"));
    assert!(text.contains("STALE (claimable)"));
    assert!(text.contains("§ Uncovered heading"));
    assert!(!text.contains("released scope must disappear"));
    assert!(text.contains("entries=1"));
    assert!(text.contains("next: board show P7@1 -b 32768"));
    assert!(budget.fits(&text));
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

#[test]
fn default_review_budget_bounds_proposals_commits_tasks_and_claims() {
    use crate::board::{
        board_ids::TaskId,
        board_vocabulary::{PlanText, PlanTitle, TaskColumn},
        review_packet::CrossedCommit,
    };
    let mut packet = review_packet();
    let proposal_line = "proposal includes concrete facts\n";
    let proposal_body = proposal_line.repeat(32768 / proposal_line.len());
    let proposal_lines = proposal_body.lines().count();
    packet.open_proposals.push(ProposalRecord {
        entry: EntryId::new(80).unwrap(),
        plan: packet.plan.id,
        base_revision: 1,
        body: PlanText::new(proposal_body).unwrap(),
        state: ProposalState::Open,
        decision_entry: None,
        result_revision: None,
    });
    for ordinal in 1..=40 {
        let task = TaskId::new(packet.plan.id, ordinal).unwrap();
        packet.tasks.push(TaskRecord {
            id: task,
            title: PlanTitle::new(format!("Review task {ordinal}")).unwrap(),
            column: TaskColumn::Doing,
            assignee: None,
            section: None,
            seq: EventSeq::new(ordinal),
        });
        packet.claims.push(ClaimRecord {
            task,
            actor: actor(),
            entry: EntryId::new(ordinal).unwrap(),
            scope: EntryText::new("verify concrete scope and evidence ".repeat(8)).unwrap(),
            claimed_at: ordinal as i64,
            last_active: ordinal as i64,
            ended_at: None,
            end_reason: None,
            stale: false,
            model: Some("claimed-model".into()),
            effort: Some("xhigh".into()),
        });
        let linked = review_commit(100 + ordinal);
        packet.crossed.push(CrossedCommit {
            repo_key: linked.commit.repo_key.clone(),
            oid: linked.commit.oid,
            task,
            claimant: actor(),
            claim_entry: EntryId::new(ordinal).unwrap(),
            scope: EntryText::new("crossed scope evidence").unwrap(),
        });
        packet.linked.push(linked);
        packet.unlinked.push(review_commit(140 + ordinal));
    }
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(4000).unwrap().with_format(format);
        let rendered = render_review(&packet, &budget, "local").unwrap();
        assert!(budget.fits(&rendered.text));
        assert!(!rendered.text.contains(proposal_line.trim()));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            let data = &value["result"]["data"];
            assert_eq!(data["open_proposals"][0]["body_lines"], proposal_lines);
            assert_eq!(
                data["open_proposals"][0]["drill"],
                "board show E80 -b 32768"
            );
            assert!(data["open_proposals"][0]["body"].is_null());
            for (records, count) in [
                ("linked", "linked_commits"),
                ("unlinked", "unlinked_commits"),
                ("tasks", "tasks"),
                ("claims", "claims"),
                ("crossed", "crossed_commits"),
            ] {
                let shown = data[records].as_array().unwrap().len();
                let omitted = data["omitted"][count].as_u64().unwrap() as usize;
                assert_eq!(shown + omitted, 40, "{records}");
                assert!(shown > 0, "preserve useful {records} evidence");
            }
            for field in ["linked", "unlinked"] {
                let visible = data[field].as_array().unwrap();
                let total = if field == "linked" { 140 } else { 180 };
                assert_eq!(visible.last().unwrap()["oid"], format!("{total:040x}"));
            }
            assert!(data["next"].as_str().unwrap().contains("-b"));
        } else {
            assert!(rendered.text.contains(&format!(
                "E80 base=@1 ({proposal_lines} lines; board show E80"
            )));
            assert!(rendered.text.contains("linked_commits="));
            assert!(rendered.text.contains("next: board review P7@1 -b"));
            assert!(
                rendered
                    .text
                    .contains("stale base; rebase before acceptance")
            );
        }
    }
}

#[test]
fn review_refits_recent_entries_after_trimming_diff() {
    use crate::board::review_packet::SsotHunk;
    let mut packet = review_packet();
    let mut recent = entry(100);
    recent.kind = EntryKind::Progress;
    recent.body = EntryText::new("recent useful review evidence").unwrap();
    packet.entries.push(recent);
    packet.ssot_diff.hunks.push(SsotHunk {
        before_start: 1,
        after_start: 1,
        removed: vec!["old individual evidence words\n".repeat(1000)],
        added: vec!["new individual evidence words\n".repeat(1000)],
        context_before: Vec::new(),
        context_after: Vec::new(),
    });
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(1000).unwrap().with_format(format);
        let rendered = render_review(&packet, &budget, "local").unwrap();
        assert!(rendered.text.contains("recent useful review evidence"));
        assert!(budget.fits(&rendered.text));
    }
}

#[test]
fn busy_inbox_uses_the_wire_state_name_in_lines() {
    let mut feed = inbox(false);
    feed.events.clear();
    feed.wait = InboxWait::Busy;
    let budget = OutputBudget::new(800)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered =
        render_reply(&BoardReply::new("local", BoardResult::Inbox(feed)), &budget).unwrap();
    assert!(rendered.text.contains("wait=busy\n"));
}

#[test]
fn minimum_review_budget_error_identifies_the_required_packet() {
    let error =
        render_review(&review_packet(), &OutputBudget::new(1).unwrap(), "local").unwrap_err();
    assert!(error.to_string().contains("minimal review packet"));
    assert!(error.to_string().contains("raise -b"));
}

#[test]
fn committed_change_receipt_survives_a_tiny_output_budget() {
    use crate::board::board_ids::{PlanRevision, TaskId};
    let reply = BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry: EntryId::new(9).unwrap(),
            seq: EventSeq::new(9),
            plan: Some(PlanId::new(7).unwrap()),
            revision: Some(PlanRevision::new(PlanId::new(7).unwrap(), 2).unwrap()),
            task: Some(TaskId::new(PlanId::new(7).unwrap(), 3).unwrap()),
            deduplicated: false,
        }),
    );
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(1).unwrap().with_format(format);
        let rendered = render_reply(&reply, &budget).unwrap();
        assert!(rendered.text.contains("committed"));
        assert!(rendered.text.contains("E9"));
        assert!(rendered.text.contains("do not repeat"));
        assert!(rendered.text.contains("board show P7 -b 1500"));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            assert_eq!(value["result"]["data"]["revision"], "P7@2");
            assert_eq!(value["result"]["data"]["task"], "P7.3");
        } else {
            assert!(rendered.text.contains("revision=P7@2 task=P7.3"));
        }
        assert!(OutputBudget::new(1500).unwrap().fits(&rendered.text));
    }
}

#[test]
fn review_recovery_hint_uses_the_accepted_cli_grammar() {
    let mut packet = review_packet();
    packet.agent = Some(HarnessLabel::parse("codex").unwrap());
    packet.omitted.entries = 1;
    let budget = OutputBudget::new(1000).unwrap();
    let rendered = render_review(&packet, &budget, "local").unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let hint = value["result"]["data"]["next"].as_str().unwrap();
    let args = hint
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let options = crate::cli::parse(&args).unwrap();
    let command = crate::board::board_grammar::parse(&options, None).unwrap();
    assert!(
        matches!(command, crate::board::board_grammar::BoardCommand::Op(BoardOp::Review { base, agent: Some(agent) })
        if base == packet.base && agent.as_str() == "codex")
    );
}

#[test]
fn review_commit_trimming_uses_one_chronological_window() {
    let mut packet = review_packet();
    packet.linked = (101..104).map(review_commit).collect();
    packet.unlinked = (1..41).map(review_commit).collect();
    let budget = OutputBudget::new(1500).unwrap();
    let rendered = render_review(&packet, &budget, "local").unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let data = &value["result"]["data"];
    assert_eq!(data["omitted"]["linked_commits"], 0);
    assert!(data["omitted"]["unlinked_commits"].as_u64().unwrap() > 0);
    assert_eq!(data["linked"].as_array().unwrap().len(), 3);
    let oldest = data["unlinked"].as_array().unwrap().first().unwrap()["committed_at"]
        .as_u64()
        .unwrap();
    assert!(oldest > 1);
    assert!(budget.fits(&rendered.text));
}

#[test]
fn entry_drill_keeps_large_proposal_before_long_reverse_references() {
    let body = "complete proposal evidence line\n".repeat(1000);
    let entry = entry(80);
    let related = (100..120)
        .map(|seq| {
            let mut related = self::entry(seq);
            related.kind = EntryKind::Answer;
            related.body = EntryText::new("many tokens of related evidence ".repeat(125)).unwrap();
            related
        })
        .collect::<Vec<_>>();
    let view = EntryView {
        entry: entry.clone(),
        replies: related.clone(),
        replies_omitted: 7,
        backrefs: related,
        backrefs_omitted: 9,
        proposal: Some(ProposalRecord {
            entry: entry.id,
            plan: entry.plan.unwrap(),
            base_revision: 1,
            body: super::super::board_vocabulary::PlanText::new(body.clone()).unwrap(),
            state: ProposalState::Open,
            decision_entry: None,
            result_revision: None,
        }),
        can_decide: true,
        can_supersede: true,
        can_answer: false,
        can_triage: false,
        can_close: false,
        plan_head_revision: Some(1),
    };
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(32768).unwrap().with_format(format);
        let rendered = render_reply(
            &BoardReply::new("local", BoardResult::Entry(view.clone())),
            &budget,
        )
        .unwrap();
        assert!(budget.fits(&rendered.text));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            assert_eq!(value["result"]["data"]["proposal"]["body"], body);
            let replies = value["result"]["data"]["replies"].as_array().unwrap().len();
            let backrefs = value["result"]["data"]["backrefs"]
                .as_array()
                .unwrap()
                .len();
            assert!(replies + backrefs < 40);
            assert_eq!(value["result"]["data"]["replies_omitted"], 27 - replies);
            assert_eq!(value["result"]["data"]["backrefs_omitted"], 29 - backrefs);
            assert_eq!(value["omitted"]["body_lines"], 0);
        } else {
            assert_eq!(
                rendered
                    .text
                    .matches("| complete proposal evidence line")
                    .count(),
                1000
            );
            assert!(rendered.text.contains("omitted: replies="));
        }
        let short = render_reply(
            &BoardReply::new("local", BoardResult::Entry(view.clone())),
            &OutputBudget::new(600).unwrap().with_format(format),
        )
        .unwrap();
        assert!(short.text.contains("board show E80 -b 32768"));
    }
}
