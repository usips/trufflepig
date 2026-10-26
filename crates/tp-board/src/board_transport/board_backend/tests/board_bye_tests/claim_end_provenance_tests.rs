use super::*;
use crate::board_actor::BoardActor;
use crate::board_ids::{BoardRef, EntryId, PlanId, TaskId};
use crate::board_protocol::{
    BoardOp, BoardReply, BoardRequest, BoardResult, ByeOutcome, ClaimResume, ReadScope,
    WorkerAddress, WorkerHandle,
};
use crate::board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle, WorkerRole};
use std::time::Duration;

fn call(fixture: &ByeFixture, actor: &BoardActor, op: BoardOp) -> BoardReply {
    fixture
        .host
        .handle_by(
            &BoardRequest::new(actor.clone(), op),
            &QueryDeadline::after(Duration::from_secs(5)),
        )
        .unwrap()
}

fn new_plan(fixture: &ByeFixture, actor: &BoardActor, title: &str) -> PlanId {
    let reply = call(
        fixture,
        actor,
        BoardOp::New {
            title: PlanTitle::new(title).unwrap(),
            body: PlanText::new("plan body").unwrap(),
            repo_key: None,
        },
    );
    let BoardResult::Change(change) = reply.result else {
        panic!("expected plan creation change");
    };
    change.plan.expect("new plan has a plan ID")
}

fn new_task(fixture: &ByeFixture, actor: &BoardActor, plan: PlanId, title: &str) -> TaskId {
    let reply = call(
        fixture,
        actor,
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new(title).unwrap(),
            to: None,
            section: None,
        },
    );
    let BoardResult::Change(change) = reply.result else {
        panic!("expected task creation change");
    };
    change.task.expect("new task has a task ID")
}

fn claim(fixture: &ByeFixture, actor: &BoardActor, task: TaskId) -> EntryId {
    let reply = call(
        fixture,
        actor,
        BoardOp::ClaimTask {
            task,
            scope: Some(EntryText::new("lead scope").unwrap()),
            resume: ClaimResume::No,
            delegate: None,
        },
    );
    let BoardResult::Change(change) = reply.result else {
        panic!("expected claim change");
    };
    change.entry
}

#[test]
fn bye_links_each_claim_end_to_its_start_and_is_visible_in_each_affected_plan_feed() {
    let fixture = ByeFixture::new();
    let config = BoardConfig::for_database(&fixture.database);
    let lead = config.actor(Some("claude"), Some("lead-session")).unwrap();
    call(
        &fixture,
        &lead,
        BoardOp::Hello {
            model: "opus".into(),
            effort: None,
            worker: None,
        },
    );

    let first_plan = new_plan(&fixture, &lead, "First plan");
    let first_task = new_task(&fixture, &lead, first_plan, "First task");
    let second_task = new_task(&fixture, &lead, first_plan, "Second task");
    let second_plan = new_plan(&fixture, &lead, "Second plan");
    let third_task = new_task(&fixture, &lead, second_plan, "Third task");
    let claims = [
        (first_plan, first_task, claim(&fixture, &lead, first_task)),
        (first_plan, second_task, claim(&fixture, &lead, second_task)),
        (second_plan, third_task, claim(&fixture, &lead, third_task)),
    ];

    let reply = call(&fixture, &lead, BoardOp::Bye);
    let BoardResult::Bye(bye) = reply.result else {
        panic!("expected bye result");
    };
    assert_eq!(bye.outcome, ByeOutcome::Ended);
    let bye_entry = bye.entry.expect("ended bye has an entry");
    let bye_seq = bye.seq.expect("ended bye has a sequence");
    let conn = rusqlite::Connection::open(&fixture.database).unwrap();

    for plan in [first_plan, second_plan] {
        let reply = call(
            &fixture,
            &lead,
            BoardOp::Feed {
                plan: Some(plan),
                scope: ReadScope::All,
                after: None,
                through: None,
                limit: 50,
            },
        );
        let BoardResult::Feed(feed) = reply.result else {
            panic!("expected plan feed");
        };
        let bye_events = feed
            .events
            .iter()
            .filter(|event| event.kind == EntryKind::Bye)
            .collect::<Vec<_>>();
        assert_eq!(
            bye_events.len(),
            1,
            "{plan} should expose the shared bye once"
        );
        let event = bye_events[0];
        assert_eq!(event.plan, None);
        assert_eq!(event.subject, BoardRef::Entry(bye_entry));
        assert_eq!(event.seq, bye_seq);
        assert_eq!(event.actor, lead);
    }

    for (plan, task, claim_start) in claims {
        let reply = call(
            &fixture,
            &lead,
            BoardOp::Entries {
                plan: Some(plan),
                kind: Some(EntryKind::Claim),
                harness: None,
                user: None,
                host: None,
                task: Some(task),
                references: Some(claim_start),
                after: None,
                before: None,
                through: Some(bye_seq),
                limit: 50,
            },
        );
        let BoardResult::Entries(page) = reply.result else {
            panic!("expected task entries");
        };
        let end_entries = page
            .entries
            .iter()
            .filter(|entry| entry.refs.contains(&BoardRef::Entry(claim_start)))
            .collect::<Vec<_>>();
        assert_eq!(
            end_entries.len(),
            1,
            "{task} should have one linked end entry"
        );
        let end = end_entries[0];
        assert_eq!(end.plan, Some(plan));
        assert_eq!(end.kind, EntryKind::Claim);
        assert_eq!(end.seq, bye_seq);
        assert_eq!(end.actor, lead);
        assert!(end.refs.contains(&BoardRef::Task(task)));
        assert!(end.refs.contains(&BoardRef::Entry(bye_entry)));
        assert!(end.body.as_str().contains("reason=session_ended"));
        let (ended_at, end_reason): (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT ended_at,end_reason FROM claims WHERE entry_id=?1",
                [i64::try_from(claim_start.get()).unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(end_reason.as_deref(), Some("session_ended"));
        assert_eq!(ended_at, Some(end.created_at));
    }

    let rows_after_bye = fixture.rows();
    let repeated = call(&fixture, &lead, BoardOp::Bye);
    let BoardResult::Bye(repeated) = repeated.result else {
        panic!("expected repeated bye result");
    };
    assert_eq!(repeated.outcome, ByeOutcome::AlreadyEnded);
    assert_eq!(repeated.entry, Some(bye_entry));
    assert_eq!(repeated.seq, Some(bye_seq));
    assert_eq!(fixture.rows(), rows_after_bye);
}

#[test]
fn child_worker_bye_does_not_end_the_lead_claim() {
    let fixture = ByeFixture::new();
    let config = BoardConfig::for_database(&fixture.database);
    let lead = config.actor(Some("claude"), Some("lead-session")).unwrap();
    call(
        &fixture,
        &lead,
        BoardOp::Hello {
            model: "opus".into(),
            effort: None,
            worker: None,
        },
    );
    let plan = new_plan(&fixture, &lead, "Worker bye plan");
    let task = new_task(&fixture, &lead, plan, "Lead task");
    let claim_start = claim(&fixture, &lead, task);
    let handle = WorkerHandle::parse("child").unwrap();
    let worker_address = WorkerAddress::new(task, handle.clone()).unwrap();
    call(
        &fixture,
        &lead,
        BoardOp::WorkerAdd {
            task,
            handle,
            role: WorkerRole::Code,
            model: "gpt-6-luna".into(),
            effort: Some("xhigh".into()),
            scope: EntryText::new("worker scope").unwrap(),
            files: vec!["crates/tp-board/src/lib.rs".into()],
        },
    );

    let child = config.actor(Some("claude"), Some("child-session")).unwrap();
    call(
        &fixture,
        &child,
        BoardOp::Hello {
            model: "gpt-6-luna".into(),
            effort: Some("xhigh".into()),
            worker: Some(worker_address),
        },
    );
    let child_bye = call(&fixture, &child, BoardOp::Bye);
    let BoardResult::Bye(child_bye) = child_bye.result else {
        panic!("expected child bye result");
    };
    assert_eq!(child_bye.outcome, ByeOutcome::Ended);

    let conn = rusqlite::Connection::open(&fixture.database).unwrap();
    let claim_row: (Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT ended_at,end_reason FROM claims WHERE entry_id=?1",
            [i64::try_from(claim_start.get()).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(claim_row, (None, None));
    let end_entries: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entries WHERE plan_id=?1 AND kind='claim' AND body LIKE '%reason=session_ended%'",
            [i64::try_from(plan.get()).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(end_entries, 0);
}
