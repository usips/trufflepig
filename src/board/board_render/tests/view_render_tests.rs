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
        done_at: None,
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
        repo_keys: Vec::new(),
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
        repo_keys: Vec::new(),
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
    assert_eq!(value["omitted"]["entries"], 4 - page.entries.len());
}
