use super::*;
use crate::board::board_ids::TaskId;
use crate::board::board_vocabulary::{PlanTitle, TaskColumn};

fn overview_with_recent_done() -> BoardReply {
    let plan = PlanId::new(7).unwrap();
    let task = |ordinal, column| TaskRecord {
        id: TaskId::new(plan, ordinal).unwrap(),
        title: PlanTitle::new("recent completion evidence ".repeat(9)).unwrap(),
        column,
        assignee: None,
        section: None,
        seq: EventSeq::new(ordinal),
        done_at: (column == TaskColumn::Done).then_some(600),
    };
    BoardReply::new(
        "local",
        BoardResult::Overview(OverviewReply {
            plans: vec![PlanOverview {
                repo_keys: Vec::new(),
                plan: PlanRecord {
                    id: plan,
                    title: PlanTitle::new("Trial").unwrap(),
                    owner_user: "josh".into(),
                    steward: None,
                    head_revision: 1,
                    created_at: 100,
                },
                tasks: vec![task(1, TaskColumn::Todo)],
                task_ceiling: TaskCeiling { plan, ordinal: 30 },
                done_count: 25,
                recent_done: (21..=25)
                    .rev()
                    .map(|ordinal| task(ordinal, TaskColumn::Done))
                    .collect(),
                claims: Vec::new(),
                open_questions: 2,
                open_proposals: 3,
                open_feedback: 4,
                tasks_omitted: 4,
                claims_omitted: 2,
            }],
            omitted: 0,
            scope: ReadScope::All,
            server_now: 100,
            claim_ttl_secs: 60,
            after: None,
            through: EventSeq::new(70),
            next_after: None,
        }),
    )
}

#[test]
fn overview_budget_drops_recent_done_and_keeps_count() {
    let reply = overview_with_recent_done();
    let budget = OutputBudget::new(400).unwrap();
    let mut metadata = reply.clone();
    let BoardResult::Overview(page) = &mut metadata.result else {
        panic!("overview");
    };
    page.plans[0].tasks.clear();
    page.plans[0].recent_done.clear();
    page.plans[0].tasks_omitted = 5;
    render_reply(&metadata, &budget).expect("overview metadata fits this budget");

    let rendered = render_reply(&reply, &budget).expect("nested fallback fits this budget");
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let page: OverviewReply = serde_json::from_value(value["result"]["data"].clone()).unwrap();
    let overview = &page.plans[0];
    assert!(overview.tasks.is_empty());
    assert!(overview.recent_done.is_empty());
    assert_eq!(overview.done_count, 25);
    assert_eq!(overview.tasks_omitted, 5);
    assert_eq!(overview.claims_omitted, 2);
    assert_eq!(overview.task_ceiling.ordinal, 30);
    assert_eq!(page.through, EventSeq::new(70));
    assert!(budget.fits(&rendered.text));
}

#[test]
fn overview_budget_keeps_recent_done_when_details_fit() {
    let reply = overview_with_recent_done();
    let budget = OutputBudget::new(2_000).unwrap();
    let rendered = render_reply(&reply, &budget).unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let page: OverviewReply = serde_json::from_value(value["result"]["data"].clone()).unwrap();
    let overview = &page.plans[0];
    assert_eq!(overview.tasks.len(), 1);
    assert_eq!(overview.recent_done.len(), 5);
    assert_eq!(overview.done_count, 25);
    assert_eq!(overview.tasks_omitted, 4);
    assert!(
        overview
            .recent_done
            .iter()
            .all(|task| task.done_at == Some(600))
    );
    assert!(budget.fits(&rendered.text));
}
