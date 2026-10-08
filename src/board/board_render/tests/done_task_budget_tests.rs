use super::*;
use crate::board::board_ids::TaskId;
use crate::board::board_vocabulary::{PlanTitle, TaskColumn};

fn done_tasks() -> Vec<TaskRecord> {
    let plan = PlanId::new(7).unwrap();
    (1..=40)
        .rev()
        .map(|number| TaskRecord {
            id: TaskId::new(plan, number).unwrap(),
            title: PlanTitle::new("completion with useful evidence ".repeat(7)).unwrap(),
            column: TaskColumn::Done,
            assignee: None,
            section: None,
            seq: EventSeq::new(number + 100),
            done_at: Some(1700000000),
        })
        .collect()
}

fn done_pages() -> [BoardResult; 2] {
    let plan = PlanId::new(7).unwrap();
    [
        BoardResult::Tasks(TaskPage {
            plan,
            column: Some(TaskColumn::Done),
            order: TaskOrder::RecentFirst,
            tasks: done_tasks(),
            after: None,
            before: None,
            ceiling: TaskCeiling { plan, ordinal: 40 },
            through: EventSeq::new(200),
            next_after: None,
            next_before: None,
            omitted: 0,
        }),
        BoardResult::DoneTasks(DoneTasksPage {
            scope: ReadScope::All,
            tasks: done_tasks(),
            before: None,
            next_before: None,
            omitted: 0,
            server_now: 1700000000,
        }),
    ]
}

#[test]
fn done_task_budget_updates_before_to_last_shown() {
    for result in done_pages() {
        let mut reply = BoardReply::new("local", result);
        reply.snapshot_seq = Some(EventSeq::new(200));
        let budget = OutputBudget::new(1200).unwrap();
        let rendered = render_reply(&reply, &budget).unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
        let page = &value["result"]["data"];
        let shown = page["tasks"].as_array().unwrap();
        assert!(!shown.is_empty() && shown.len() < 40);
        let last = shown.last().unwrap();
        assert_eq!(page["next_before"]["id"], last["id"]);
        assert_eq!(page["next_before"]["seq"], last["seq"]);
        assert_eq!(page["omitted"], 40 - shown.len());
        assert!(value["next"].as_str().unwrap().contains(&format!(
            "--after {}:{}",
            last["seq"],
            last["id"].as_str().unwrap()
        )));
        assert!(budget.fits(&rendered.text));
    }
}

#[test]
fn done_task_budget_preserves_and_quotes_the_original_project_selector() {
    for result in done_pages() {
        let plan = if matches!(result, BoardResult::Tasks(_)) {
            " P7"
        } else {
            ""
        };
        let mut reply = BoardReply::new("local", result);
        reply.snapshot_seq = Some(EventSeq::new(200));
        let budget = OutputBudget::new(1200).unwrap();
        let rendered = render_cli_reply(&reply, &budget, Some("Space's Fleet")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
        let shown = value["result"]["data"]["tasks"].as_array().unwrap();
        assert!(!shown.is_empty() && shown.len() < 40);
        let last = shown.last().unwrap();
        assert_eq!(
            value["next"].as_str().unwrap(),
            format!(
                "board done{plan} --after {}:{} --project 'Space'\\''s Fleet'",
                last["seq"],
                last["id"].as_str().unwrap(),
            )
        );
        assert!(budget.fits(&rendered.text));
    }
}

#[test]
fn done_task_lines_include_completion_and_continuation() {
    let cursor = TaskCursor {
        seq: EventSeq::new(101),
        id: "P7.1".parse().unwrap(),
    };
    let reply = BoardReply::new(
        "local",
        BoardResult::DoneTasks(DoneTasksPage {
            scope: ReadScope::All,
            tasks: vec![done_tasks().pop().unwrap()],
            before: None,
            next_before: Some(cursor),
            omitted: 1,
            server_now: 1700000000,
        }),
    );
    let budget = OutputBudget::new(1200)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = render_reply(&reply, &budget).unwrap();
    assert!(rendered.text.contains("completed=1700000000"));
    assert!(
        rendered
            .text
            .contains("next: board done --after 101:P7.1 --all")
    );
}
