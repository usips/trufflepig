use super::*;

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
        delegated_by: None,
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
        task_ceiling: TaskCeiling { plan, ordinal: 4 },
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
        commits: Vec::new(),
        tasks_omitted: 0,
        claims_omitted: 0,
        entries_omitted: 0,
        commits_omitted: 0,
        tasks_next_after: None,
        claims_next_after: None,
        entries_next_after: None,
        entries_next_before: None,
        through: EventSeq::new(100),
        can_edit: true,
        server_now: 150,
        claim_ttl_secs: 60,
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

#[test]
fn plan_trim_keeps_newest_entries_behind_a_before_cursor() {
    use crate::board::{
        board_ids::PlanRevision,
        board_vocabulary::{PlanText, PlanTitle},
    };
    let plan = PlanId::new(7).unwrap();
    let entries = [104, 103, 102, 101]
        .map(|seq| {
            let mut record = entry(seq);
            record.body = EntryText::new("trimmed plan evidence ".repeat(150)).unwrap();
            record
        })
        .to_vec();
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
            body: PlanText::new("# Scope\n").unwrap(),
            source: RevisionSource::Create,
            entry: EntryId::new(1).unwrap(),
            actor: actor(),
            seq: EventSeq::new(1),
            created_at: 100,
        },
        tasks: Vec::new(),
        task_ceiling: TaskCeiling { plan, ordinal: 0 },
        claims: Vec::new(),
        entries,
        commits: Vec::new(),
        tasks_omitted: 0,
        claims_omitted: 0,
        entries_omitted: 0,
        commits_omitted: 0,
        tasks_next_after: None,
        claims_next_after: None,
        entries_next_after: None,
        entries_next_before: None,
        through: EventSeq::new(104),
        can_edit: true,
        server_now: 150,
        claim_ttl_secs: 60,
        sections_without_tasks: Vec::new(),
    };
    let budget = OutputBudget::new(1600)
        .unwrap()
        .with_format(OutputFormat::Json);
    let rendered = render_reply(&BoardReply::new("local", BoardResult::Plan(view)), &budget)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    let page: PlanView = serde_json::from_value(value["result"]["data"].clone()).unwrap();
    assert!(!page.entries.is_empty() && page.entries.len() < 4);
    let oldest = page.entries.last().unwrap();
    assert_eq!(
        page.entries_next_before,
        Some(EntryCursor {
            seq: oldest.seq,
            entry: oldest.id,
        })
    );
    assert!(page.entries_next_after.is_none());
    assert_eq!(value["omitted"]["entries"], 4 - page.entries.len());
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
        replies_next_after: None,
        backrefs_next_after: None,
        through: EventSeq::new(119),
        feedback: None,
        linked_commit: None,
        proposal: Some(ProposalRecord {
            entry: entry.id,
            plan: entry.plan.unwrap(),
            base_revision: 1,
            body: crate::board::board_vocabulary::PlanText::new(body.clone()).unwrap(),
            state: ProposalState::Open,
            decision_entry: None,
            result_revision: None,
            stale_base: false,
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

#[test]
fn spooled_entry_lines_mark_imported_claims_unverified() {
    let mut imported = entry(1);
    imported.via = Some(FeedbackVia::Outbox);
    let mut rendered = String::new();
    entry_line(&mut rendered, &imported);
    assert!(rendered.contains("via=outbox spooled unverified"));
    imported.via = None;
    rendered.clear();
    entry_line(&mut rendered, &imported);
    assert!(!rendered.contains("spooled"));
}
