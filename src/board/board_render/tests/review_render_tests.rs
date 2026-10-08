use super::*;

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
        stale_base: true,
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
            done_at: None,
        });
        packet.claims.push(ClaimRecord {
            task,
            actor: actor(),
            vendor: AgentVendor::Codex,
            entry: EntryId::new(ordinal).unwrap(),
            scope: EntryText::new("verify concrete scope and evidence ".repeat(8)).unwrap(),
            claimed_at: ordinal as i64,
            last_active: ordinal as i64,
            ended_at: None,
            end_reason: None,
            stale: false,
            model: Some("claimed-model".into()),
            effort: Some("xhigh".into()),
            delegated_by: None,
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
fn minimum_review_budget_error_identifies_the_required_packet() {
    let error =
        render_review(&review_packet(), &OutputBudget::new(1).unwrap(), "local").unwrap_err();
    assert!(error.to_string().contains("minimal review packet"));
    assert!(error.to_string().contains("raise -b"));
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
fn review_budget_keeps_each_manual_link_set_with_its_commit() {
    use crate::board::{
        board_ids::{EntryId, EventSeq, TaskId},
        board_protocol::ManualCommitLink,
    };

    let mut packet = review_packet();
    let mut linked = review_commit(101);
    linked.manual_links = (1..=80)
        .map(|ordinal| ManualCommitLink {
            repo_key: linked.commit.repo_key.clone(),
            oid: linked.commit.oid,
            task: TaskId::new(packet.plan.id, ordinal).unwrap(),
            entry: EntryId::new(ordinal).unwrap(),
            seq: Some(EventSeq::new(ordinal)),
            actor: Some(actor()),
        })
        .collect();
    packet.linked.push(linked);

    let budget = OutputBudget::new(600)
        .unwrap()
        .with_format(OutputFormat::Json);
    let rendered = render_review(&packet, &budget, "local").unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let data = &value["result"]["data"];
    let shown = data["linked"].as_array().unwrap();
    let omitted = data["omitted"]["linked_commits"].as_u64().unwrap() as usize;
    assert_eq!(shown.len() + omitted, 1);
    if let Some(commit) = shown.first() {
        assert_eq!(commit["manual_links"].as_array().unwrap().len(), 80);
    }
    assert!(budget.fits(&rendered.text));
}

#[test]
fn review_json_keeps_linked_by_null_when_historical_attribution_is_unknown() {
    use crate::board::{board_ids::TaskId, board_protocol::ManualCommitLink};

    let mut packet = review_packet();
    let mut linked = review_commit(101);
    linked.manual_links.push(ManualCommitLink {
        repo_key: linked.commit.repo_key.clone(),
        oid: linked.commit.oid,
        task: TaskId::new(packet.plan.id, 1).unwrap(),
        entry: EntryId::new(10).unwrap(),
        seq: None,
        actor: None,
    });
    packet.linked.push(linked);
    let rendered = render_review(&packet, &OutputBudget::new(2000).unwrap(), "local").unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let manual_link = &value["result"]["data"]["linked"][0]["manual_links"][0];
    assert_eq!(manual_link["task"], "P7.1");
    assert_eq!(manual_link["entry"], "E10");
    assert_eq!(manual_link["seq"], serde_json::Value::Null);
    assert_eq!(manual_link.get("linked_by"), Some(&serde_json::Value::Null));
    assert!(manual_link.get("actor").is_none());
}
