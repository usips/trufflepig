use super::*;
use crate::board_actor::BoardRecipient;
use crate::board_protocol::{
    ClaimResume, PendingNoticeReason, WorkerAddress, WorkerEndReason, WorkerHandle, WorkerRole,
};

fn create_task(board: &mut LocalBoard, plan: PlanId) -> TaskId {
    let BoardResult::Change(change) = call(
        board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Addressed task question").unwrap(),
            to: None,
            section: None,
        },
    ) else {
        panic!("expected task receipt");
    };
    change
        .task
        .expect("task creation includes its task reference")
}

fn task_question(board: &mut LocalBoard, task: TaskId) -> crate::board_ids::EntryId {
    task_question_to(board, task, None, "Question with no task name in its prose")
}

fn task_question_to(
    board: &mut LocalBoard,
    task: TaskId,
    to: Option<&str>,
    body: &str,
) -> crate::board_ids::EntryId {
    let BoardResult::Change(change) = call(
        board,
        "claude",
        BoardOp::Post {
            target: BoardRef::Task(task),
            kind: EntryKind::Question,
            body: EntryText::new(body).unwrap(),
            to: to.map(|value| BoardRecipient::parse(value).unwrap()),
            supersedes: None,
            progress: None,
            verdict: None,
        },
    ) else {
        panic!("expected question receipt");
    };
    change.entry
}

fn pending(board: &mut LocalBoard, harness: &str, after: EventSeq) -> PendingReply {
    match call(
        board,
        harness,
        BoardOp::Pending {
            after: Some(after),
            limit: 20,
        },
    ) {
        BoardResult::Pending(reply) => reply,
        other => panic!("unexpected {other:?}"),
    }
}

fn has_question(reply: &PendingReply, entry: crate::board_ids::EntryId) -> bool {
    reply.notices.iter().any(|notice| {
        notice
            .entry
            .as_ref()
            .is_some_and(|record| record.id == entry)
    })
}

fn addressed_note_event(board: &mut LocalBoard, plan: PlanId, body: &str) -> EventSeq {
    let BoardResult::Change(change) = call(
        board,
        "claude",
        BoardOp::Post {
            target: BoardRef::Plan(plan),
            kind: EntryKind::Note,
            body: EntryText::new(body).unwrap(),
            to: Some(BoardRecipient::parse("human").unwrap()),
            supersedes: None,
            progress: None,
            verdict: None,
        },
    ) else {
        panic!("expected post receipt");
    };
    change.seq
}

#[test]
fn open_task_questions_follow_the_current_holder_and_remain_with_the_owner() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let task = create_task(&mut board, plan);
    let question = task_question(&mut board, task);
    let question_seq = board
        .conn
        .query_row(
            "SELECT seq FROM entries WHERE id=?1",
            [question.get() as i64],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let after = EventSeq::new(question_seq as u64);

    let before_pending = board.max_seq().unwrap();
    let owner = pending(&mut board, "human", after);
    assert!(has_question(&owner, question));
    assert!(owner.notices.iter().any(|notice| {
        notice
            .entry
            .as_ref()
            .is_some_and(|entry| entry.id == question)
            && notice.reason == PendingNoticeReason::Question
    }));
    assert!(!has_question(
        &pending(&mut board, "claude", after),
        question
    ));
    assert_eq!(board.max_seq().unwrap(), before_pending);

    call(
        &mut board,
        "muse",
        BoardOp::Hello {
            model: "Muse".into(),
            effort: None,
            worker: None,
        },
    );
    assert!(!has_question(&pending(&mut board, "muse", after), question));

    call(
        &mut board,
        "codex",
        BoardOp::ClaimTask {
            task,
            scope: Some(EntryText::new("first holder").unwrap()),
            resume: ClaimResume::No,
            delegate: None,
        },
    );
    let first_holder = pending(&mut board, "codex", after);
    assert!(has_question(&first_holder, question));
    let handle = WorkerHandle::parse("helper").unwrap();
    call(
        &mut board,
        "codex",
        BoardOp::WorkerAdd {
            task,
            handle: handle.clone(),
            role: WorkerRole::Code,
            model: "worker model".into(),
            effort: None,
            scope: EntryText::new("answer the task question").unwrap(),
            files: vec!["src/task.rs".into()],
        },
    );
    call(
        &mut board,
        "grok",
        BoardOp::Hello {
            model: "worker model".into(),
            effort: None,
            worker: Some(WorkerAddress::new(task, handle).unwrap()),
        },
    );
    let worker_pending = pending(&mut board, "grok", after);
    assert!(has_question(&worker_pending, question));
    assert!(worker_pending.notices.iter().any(|notice| {
        notice.reason == PendingNoticeReason::Worker
            && notice.event.as_ref().is_some_and(|event| {
                event.kind == EntryKind::WorkerAdd && event.subject == BoardRef::Task(task)
            })
    }));
    let bound_holder = pending(&mut board, "codex", after);
    assert_ne!(bound_holder.generation, first_holder.generation);
    call(
        &mut board,
        "codex",
        BoardOp::Post {
            target: BoardRef::Task(task),
            kind: EntryKind::Note,
            body: EntryText::new("routine task activity").unwrap(),
            to: None,
            supersedes: None,
            progress: None,
            verdict: None,
        },
    );
    let claim_before_pending: i64 = board
        .conn
        .query_row(
            "SELECT last_active FROM claims WHERE ended_at IS NULL AND plan_id=?1 AND task_ordinal=?2",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        )
        .unwrap();
    let after_activity = pending(&mut board, "codex", after);
    assert_eq!(after_activity.generation, bound_holder.generation);
    let claim_after_pending: i64 = board
        .conn
        .query_row(
            "SELECT last_active FROM claims WHERE ended_at IS NULL AND plan_id=?1 AND task_ordinal=?2",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claim_after_pending, claim_before_pending);
    call(
        &mut board,
        "codex",
        BoardOp::TaskMove {
            task,
            column: crate::board_vocabulary::TaskColumn::Todo,
            to: None,
        },
    );
    call(
        &mut board,
        "muse",
        BoardOp::ClaimTask {
            task,
            scope: Some(EntryText::new("new holder").unwrap()),
            resume: ClaimResume::No,
            delegate: None,
        },
    );
    assert!(!has_question(
        &pending(&mut board, "codex", after),
        question
    ));
    assert!(!has_question(&pending(&mut board, "grok", after), question));
    assert!(has_question(&pending(&mut board, "muse", after), question));

    call(
        &mut board,
        "muse",
        BoardOp::Post {
            target: BoardRef::Task(task),
            kind: EntryKind::Answer,
            body: EntryText::new(format!("Resolved {question}")).unwrap(),
            to: None,
            supersedes: None,
            progress: None,
            verdict: None,
        },
    );
    assert!(!has_question(&pending(&mut board, "muse", after), question));
}

#[test]
fn pending_pages_new_addressed_events_before_old_open_question_reminders() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let task = create_task(&mut board, plan);
    for _ in 0..3 {
        task_question(&mut board, task);
    }
    let after_old_questions = board.max_seq().unwrap();
    let first_event = addressed_note_event(&mut board, plan, "First addressed event");
    let second_event = addressed_note_event(&mut board, plan, "Second addressed event");
    let after = EventSeq::new(after_old_questions.get());

    let first = match call(
        &mut board,
        "human",
        BoardOp::Pending {
            after: Some(after),
            limit: 1,
        },
    ) {
        BoardResult::Pending(reply) => reply,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(first.notices.len(), 1);
    assert_eq!(first.notices[0].seq, first_event);
    assert!(first.notices[0].event.is_some());

    let second = match call(
        &mut board,
        "human",
        BoardOp::Pending {
            after: Some(first_event),
            limit: 1,
        },
    ) {
        BoardResult::Pending(reply) => reply,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(second.notices.len(), 1);
    assert_eq!(second.notices[0].seq, second_event);
    assert!(second.notices[0].event.is_some());
    assert!(second.through >= second_event);

    let reminders = match call(
        &mut board,
        "human",
        BoardOp::Pending {
            after: Some(second_event),
            limit: 1,
        },
    ) {
        BoardResult::Pending(reply) => reply,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(reminders.notices.len(), 1);
    assert_eq!(reminders.notices[0].reason, PendingNoticeReason::Question);
}

#[test]
fn explicit_task_question_recipients_override_dynamic_task_routing() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let task = create_task(&mut board, plan);
    call(
        &mut board,
        "codex",
        BoardOp::ClaimTask {
            task,
            scope: Some(EntryText::new("lead work").unwrap()),
            resume: ClaimResume::No,
            delegate: None,
        },
    );
    let handle = WorkerHandle::parse("helper").unwrap();
    call(
        &mut board,
        "codex",
        BoardOp::WorkerAdd {
            task,
            handle: handle.clone(),
            role: WorkerRole::Code,
            model: "worker model".into(),
            effort: None,
            scope: EntryText::new("worker scope").unwrap(),
            files: vec!["src/task.rs".into()],
        },
    );
    call(
        &mut board,
        "grok",
        BoardOp::Hello {
            model: "worker model".into(),
            effort: None,
            worker: Some(WorkerAddress::new(task, handle).unwrap()),
        },
    );

    let human_question = task_question_to(
        &mut board,
        task,
        Some("human"),
        "Explicitly sent to the human",
    );
    let worker_question = task_question_to(
        &mut board,
        task,
        Some("grok"),
        "Explicitly sent to the worker harness",
    );
    let default_question =
        task_question_to(&mut board, task, None, "Defaults to the active task team");
    call(
        &mut board,
        "muse",
        BoardOp::Hello {
            model: "Muse".into(),
            effort: None,
            worker: None,
        },
    );
    let after = board.max_seq().unwrap();

    let owner = pending(&mut board, "human", after);
    assert!(has_question(&owner, human_question));
    assert!(has_question(&owner, worker_question));
    assert!(has_question(&owner, default_question));

    let lead = pending(&mut board, "codex", after);
    assert!(!has_question(&lead, human_question));
    assert!(!has_question(&lead, worker_question));
    assert!(has_question(&lead, default_question));

    let worker = pending(&mut board, "grok", after);
    assert!(!has_question(&worker, human_question));
    assert!(has_question(&worker, worker_question));
    assert!(has_question(&worker, default_question));

    assert!(pending(&mut board, "muse", after).notices.is_empty());
}

/// Lead `codex` claims a task, adds worker `helper`, and `grok` binds to it.
fn lead_with_bound_worker(board: &mut LocalBoard) -> (TaskId, WorkerAddress) {
    let plan = plan(board);
    let task = create_task(board, plan);
    call(
        board,
        "codex",
        BoardOp::ClaimTask {
            task,
            scope: Some(EntryText::new("lead work").unwrap()),
            resume: ClaimResume::No,
            delegate: None,
        },
    );
    let handle = WorkerHandle::parse("helper").unwrap();
    let address = WorkerAddress::new(task, handle.clone()).unwrap();
    call(
        board,
        "codex",
        BoardOp::WorkerAdd {
            task,
            handle,
            role: WorkerRole::Code,
            model: "worker model".into(),
            effort: None,
            scope: EntryText::new("worker scope").unwrap(),
            files: vec!["src/task.rs".into()],
        },
    );
    call(
        board,
        "grok",
        BoardOp::Hello {
            model: "worker model".into(),
            effort: None,
            worker: Some(address.clone()),
        },
    );
    (task, address)
}

fn has_worker_end_entry_notice(reply: &PendingReply) -> bool {
    reply.notices.iter().any(|notice| {
        notice.reason == PendingNoticeReason::Worker
            && notice.entry.as_ref().is_some_and(|entry| {
                entry.kind == EntryKind::WorkerEnd && entry.body.as_str().contains("claim_ended")
            })
    })
}

#[test]
fn explicit_worker_end_still_reaches_the_bound_worker() {
    let (_directory, mut board) = database();
    let (task, address) = lead_with_bound_worker(&mut board);
    let after = board.max_seq().unwrap();
    call(
        &mut board,
        "codex",
        BoardOp::WorkerEnd {
            worker: address,
            state: WorkerEndReason::Done,
            note: None,
        },
    );
    let worker = pending(&mut board, "grok", after);
    assert!(worker.notices.iter().any(|notice| {
        notice.reason == PendingNoticeReason::Worker
            && notice.event.as_ref().is_some_and(|event| {
                event.kind == EntryKind::WorkerEnd && event.subject == BoardRef::Task(task)
            })
    }));
}

#[test]
fn lead_release_notifies_the_bound_worker_that_its_claim_ended() {
    let (_directory, mut board) = database();
    let (task, _address) = lead_with_bound_worker(&mut board);
    let after = board.max_seq().unwrap();
    call(
        &mut board,
        "codex",
        BoardOp::TaskMove {
            task,
            column: crate::board_vocabulary::TaskColumn::Todo,
            to: None,
        },
    );
    assert!(has_worker_end_entry_notice(&pending(
        &mut board, "grok", after
    )));
    assert!(!has_worker_end_entry_notice(&pending(
        &mut board, "codex", after
    )));
}

#[test]
fn lead_bye_notifies_the_bound_worker_that_its_claim_ended() {
    let (_directory, mut board) = database();
    let (_task, _address) = lead_with_bound_worker(&mut board);
    let after = board.max_seq().unwrap();
    call(&mut board, "codex", BoardOp::Bye);
    assert!(has_worker_end_entry_notice(&pending(
        &mut board, "grok", after
    )));
}

#[test]
fn post_to_a_nonexistent_task_recipient_is_refused() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let task = create_task(&mut board, plan);
    let missing = TaskId::new(plan, task.ordinal + 1).unwrap();
    let before = board.max_seq().unwrap();
    let error = board
        .handle(&BoardRequest::new(
            BoardActor::new(
                "josh",
                "laptop",
                HarnessLabel::parse("claude").unwrap(),
                "session1",
            )
            .unwrap(),
            BoardOp::Post {
                target: BoardRef::Task(task),
                kind: EntryKind::Note,
                body: EntryText::new("nudge the missing card").unwrap(),
                to: Some(BoardRecipient::parse(&missing.to_string()).unwrap()),
                supersedes: None,
                progress: None,
                verdict: None,
            },
        ))
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board_protocol::BoardErrorCode::InvalidReference
    );
    assert_eq!(board.max_seq().unwrap(), before);
}
