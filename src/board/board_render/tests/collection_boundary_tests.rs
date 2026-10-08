use super::*;
use crate::board::board_ids::TaskId;
use crate::board::board_protocol::ReadScope;
use crate::board::board_vocabulary::{PlanTitle, TaskColumn};

fn large_entries() -> Vec<EntryRecord> {
    (11..15)
        .map(|seq| {
            let mut item = entry(seq);
            item.body = EntryText::new("visible boundary evidence ".repeat(150)).unwrap();
            item
        })
        .collect()
}

#[test]
fn feed_prefix_preserves_boundary_actor_and_outbox_warning() {
    let mut events = inbox(false).events;
    events[0].via = Some(FeedbackVia::Outbox);
    let mut reply = BoardReply::new(
        "local",
        BoardResult::Feed(EventPage {
            scope: ReadScope::All,
            plan: Some(PlanId::new(7).unwrap()),
            events,
            after: EventSeq::new(10),
            through: EventSeq::new(25),
            next_after: Some(EventSeq::new(22)),
        }),
    );
    reply.snapshot_seq = Some(EventSeq::new(30));
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(1500).unwrap().with_format(format);
        let rendered = render_reply(&reply, &budget).unwrap();
        assert!(budget.fits(&rendered.text));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            let page: EventPage = serde_json::from_value(value["result"]["data"].clone()).unwrap();
            assert!(!page.events.is_empty() && page.events.len() < 12);
            assert_eq!(page.after, EventSeq::new(10));
            assert_eq!(page.through, EventSeq::new(25));
            assert_eq!(page.next_after, page.events.last().map(|event| event.seq));
            assert_eq!(value["snapshot_seq"], 30);
            assert_eq!(value["omitted"]["events"], 12 - page.events.len());
            assert_eq!(page.events[0].via, Some(FeedbackVia::Outbox));
            assert_eq!(page.events[0].actor, actor());
            assert!(value["next"].as_str().unwrap().contains("--through 25"));
        } else {
            assert!(rendered.text.contains("via=outbox spooled unverified"));
            assert!(rendered.text.contains("gpt-6.1-sol/xhigh"));
            assert!(rendered.text.contains("next: board feed P7 --after "));
        }
    }
}

#[test]
fn internal_collections_keep_frozen_bounds_without_cli_hints() {
    let plan = PlanId::new(7).unwrap();
    let tasks = (6..36)
        .map(|ordinal| TaskRecord {
            id: TaskId::new(plan, ordinal).unwrap(),
            title: PlanTitle::new("visible task evidence ".repeat(10)).unwrap(),
            column: TaskColumn::Todo,
            assignee: None,
            section: None,
            seq: EventSeq::new(20),
            done_at: None,
        })
        .collect();
    let claims = (6..10)
        .map(|number| ClaimView {
            claim: ClaimRecord {
                task: TaskId::new(plan, number).unwrap(),
                actor: actor(),
                entry: EntryId::new(200).unwrap(),
                scope: EntryText::new("visible claim evidence ".repeat(150)).unwrap(),
                claimed_at: 100,
                last_active: 120,
                ended_at: None,
                end_reason: None,
                stale: false,
                model: None,
                effort: None,
                delegated_by: None,
            },
            cursor: ClaimCursor {
                entry: EntryId::new(200).unwrap(),
                claim: number,
            },
        })
        .collect();
    let through = EventSeq::new(222);
    let results = [
        BoardResult::Tasks(TaskPage {
            plan,
            column: None,
            order: TaskOrder::Ordinal,
            tasks,
            after: Some(TaskId::new(plan, 5).unwrap()),
            before: None,
            ceiling: TaskCeiling { plan, ordinal: 40 },
            through,
            next_after: Some(TaskId::new(plan, 35).unwrap()),
            next_before: None,
            omitted: 7,
        }),
        BoardResult::Claims(ClaimPage {
            scope: ReadScope::All,
            plan: Some(plan),
            own_stale: false,
            claims,
            after: Some(ClaimCursor {
                entry: EntryId::new(200).unwrap(),
                claim: 5,
            }),
            through,
            next_after: Some(ClaimCursor {
                entry: EntryId::new(200).unwrap(),
                claim: 9,
            }),
            omitted: 7,
            server_now: 150,
            claim_ttl_secs: 60,
        }),
        BoardResult::Entries(EntriesPage {
            plan: Some(plan),
            references: None,
            entries: large_entries(),
            after: Some(EntryCursor {
                seq: EventSeq::new(10),
                entry: EntryId::new(10).unwrap(),
            }),
            through,
            next_after: None,
            next_before: None,
        }),
    ];
    for result in results {
        let original = serde_json::to_value(&result).unwrap();
        let mut reply = BoardReply::new("local", result);
        reply.snapshot_seq = Some(EventSeq::new(300));
        let budget = OutputBudget::new(1600).unwrap();
        let rendered = render_reply(&reply, &budget).unwrap();
        assert!(budget.fits(&rendered.text));
        let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
        let page = &value["result"]["data"];
        assert_eq!(value["snapshot_seq"], 300);
        assert_eq!(page["after"], original["data"]["after"]);
        assert_eq!(page["through"], 222);
        assert!(value["next"].is_null());
        let rows = match value["result"]["result"].as_str().unwrap() {
            "tasks" => {
                assert_eq!(page["ceiling"], original["data"]["ceiling"]);
                let rows = page["tasks"].as_array().unwrap();
                assert_eq!(page["next_after"], rows.last().unwrap()["id"]);
                assert_eq!(page["omitted"], 37 - rows.len());
                rows
            }
            "claims" => {
                let rows = page["claims"].as_array().unwrap();
                assert_eq!(page["next_after"], rows.last().unwrap()["cursor"]);
                assert_eq!(page["omitted"], 11 - rows.len());
                rows
            }
            "entries" => {
                let rows = page["entries"].as_array().unwrap();
                assert_eq!(page["next_after"]["entry"], rows.last().unwrap()["id"]);
                rows
            }
            other => panic!("unexpected collection {other}"),
        };
        let original_rows = original["data"][value["result"]["result"].as_str().unwrap()]
            .as_array()
            .unwrap();
        assert!(!rows.is_empty() && rows.len() < original_rows.len());
        assert_eq!(
            value["omitted"]["entries"],
            original_rows.len() - rows.len()
        );
    }
}

#[test]
fn overview_nested_fallback_keeps_task_membership_ceiling() {
    let plan = PlanId::new(7).unwrap();
    let tasks = (1..31)
        .map(|ordinal| TaskRecord {
            id: TaskId::new(plan, ordinal).unwrap(),
            title: PlanTitle::new("nested task evidence ".repeat(10)).unwrap(),
            column: TaskColumn::Todo,
            assignee: None,
            section: None,
            seq: EventSeq::new(20),
            done_at: None,
        })
        .collect();
    let mut reply = BoardReply::new(
        "local",
        BoardResult::Overview(OverviewReply {
            scope: ReadScope::All,
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
                tasks,
                task_ceiling: TaskCeiling { plan, ordinal: 30 },
                done_count: 0,
                recent_done: Vec::new(),
                claims: Vec::new(),
                open_questions: 2,
                open_proposals: 3,
                open_feedback: 4,
                tasks_omitted: 5,
                claims_omitted: 6,
            }],
            omitted: 7,
            server_now: 100,
            claim_ttl_secs: 60,
            after: Some(PlanId::new(6).unwrap()),
            through: EventSeq::new(70),
            next_after: Some(plan),
        }),
    );
    reply.snapshot_seq = Some(EventSeq::new(100));
    let budget = OutputBudget::new(1200).unwrap();
    let rendered = render_reply(&reply, &budget).unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let page: OverviewReply = serde_json::from_value(value["result"]["data"].clone()).unwrap();
    assert_eq!(value["snapshot_seq"], 100);
    assert_eq!(page.after, Some(PlanId::new(6).unwrap()));
    assert_eq!(page.through, EventSeq::new(70));
    assert_eq!(page.next_after, Some(plan));
    assert_eq!(page.omitted, 7);
    assert_eq!(page.plans.len(), 1);
    assert_eq!(
        page.plans[0].task_ceiling,
        TaskCeiling { plan, ordinal: 30 }
    );
    assert!(page.plans[0].tasks.is_empty());
    assert_eq!(page.plans[0].tasks_omitted, 35);
    assert_eq!(page.plans[0].claims_omitted, 6);
    assert_eq!(page.plans[0].open_feedback, 4);
    assert!(
        value["next"]
            .as_str()
            .unwrap()
            .starts_with("board show --after P7 --through 70")
    );
    assert!(budget.fits(&rendered.text));
}
