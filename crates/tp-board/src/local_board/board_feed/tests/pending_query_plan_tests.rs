use super::*;
use crate::board_protocol::{BoardOp, ClaimResume, WorkerAddress, WorkerHandle, WorkerRole};
use crate::board_vocabulary::EntryText;
use rusqlite::params;
use std::collections::HashSet;

const PENDING_IDENTITY: &str = "josh@laptop/codex/session1";

#[test]
fn pending_candidate_plan_uses_task_worker_and_entry_keys() {
    let (_directory, board) = database();
    let sql = format!(
        "EXPLAIN QUERY PLAN SELECT count(*) FROM ({})",
        super::pending_candidates()
    );
    let mut statement = board.conn.prepare(&sql).unwrap();
    let details = statement
        .query_map(
            params![
                1_i64,
                PENDING_IDENTITY,
                "josh",
                "laptop",
                "codex",
                "session1",
                1_800_000_000_i64,
                0_i64,
                i64::MAX,
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .map(|row| row.unwrap())
        .collect::<Vec<_>>();

    assert!(details.iter().any(|detail| {
        detail.contains("SEARCH task_source USING COVERING INDEX sqlite_autoindex_tasks_1")
            && detail.contains("plan_id=? AND ordinal=?")
    }));
    assert!(details.iter().any(|detail| {
        detail.contains("SEARCH task_worker USING INDEX sqlite_autoindex_claim_workers_2")
            && detail.contains("plan_id=? AND task_ordinal=? AND handle=?")
    }));
    assert!(
        details
            .iter()
            .any(|detail| { detail.contains("SEARCH post USING INTEGER PRIMARY KEY (rowid=?)") })
    );
}

#[test]
fn pending_event_subject_lookup_preserves_canonical_text_matching() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "question linked by pending event subject",
        None,
    );
    call(
        &mut board,
        "codex",
        BoardOp::Hello {
            model: "Codex".into(),
            effort: None,
            worker: None,
        },
    );

    let question_id: i64 = board
        .conn
        .query_row(
            "SELECT id FROM entries WHERE plan_id=?1 AND kind='question'",
            [plan.get() as i64],
            |row| row.get(0),
        )
        .unwrap();
    let pending_actor: i64 = board
        .conn
        .query_row(
            "SELECT id FROM actors WHERE user='josh' AND host='laptop' AND harness='codex' AND session='session1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let author: i64 = board
        .conn
        .query_row(
            "SELECT id FROM actors WHERE user='josh' AND host='laptop' AND harness='claude' AND session='session1'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let exact_subject = format!("E{question_id}");
    let noncanonical_subject = format!("E{question_id}x");
    let exact_seq = insert_event(
        &board,
        plan,
        "note",
        &exact_subject,
        Some(PENDING_IDENTITY),
        author,
    );
    let noncanonical_seq = insert_event(
        &board,
        plan,
        "note",
        &noncanonical_subject,
        Some(PENDING_IDENTITY),
        author,
    );
    let event_seqs = candidate_event_seqs(&board, pending_actor, PENDING_IDENTITY);

    assert!(!event_seqs.contains(&exact_seq));
    assert!(event_seqs.contains(&noncanonical_seq));
}

#[test]
fn pending_task_recipient_requires_a_canonical_task_address() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let BoardResult::Change(created) = call(
        &mut board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Pending recipient task").unwrap(),
            to: None,
            section: None,
        },
    ) else {
        panic!("expected task receipt");
    };
    let task = created.task.unwrap();
    call(
        &mut board,
        "codex",
        BoardOp::ClaimTask {
            task,
            scope: Some(EntryText::new("verify task address lookup").unwrap()),
            resume: ClaimResume::No,
            delegate: None,
        },
    );
    let actor = actor_id(&board, "codex");
    let author = actor_id(&board, "human");
    let canonical = format!("P{}.{}", task.plan.get(), task.ordinal);
    let noncanonical = format!("P{:02}.{:02}", task.plan.get(), task.ordinal);
    let canonical_seq = insert_event(&board, plan, "note", &canonical, Some(&canonical), author);
    let noncanonical_seq = insert_event(
        &board,
        plan,
        "note",
        &canonical,
        Some(&noncanonical),
        author,
    );

    let event_seqs = candidate_event_seqs(&board, actor, PENDING_IDENTITY);

    assert!(event_seqs.contains(&canonical_seq));
    assert!(!event_seqs.contains(&noncanonical_seq));
}

#[test]
fn pending_worker_subject_requires_a_canonical_worker_address() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let BoardResult::Change(created) = call(
        &mut board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Pending worker task").unwrap(),
            to: None,
            section: None,
        },
    ) else {
        panic!("expected task receipt");
    };
    let task = created.task.unwrap();
    call(
        &mut board,
        "codex",
        BoardOp::ClaimTask {
            task,
            scope: Some(EntryText::new("verify worker address lookup").unwrap()),
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
            model: "test worker".into(),
            effort: None,
            scope: EntryText::new("verify worker address lookup").unwrap(),
            files: vec!["src/task.rs".into()],
        },
    );
    call(
        &mut board,
        "grok",
        BoardOp::Hello {
            model: "test worker".into(),
            effort: None,
            worker: Some(WorkerAddress::new(task, handle).unwrap()),
        },
    );
    let worker_actor = actor_id(&board, "grok");
    let author = actor_id(&board, "codex");
    let canonical = format!("P{}.{}/helper", task.plan.get(), task.ordinal);
    let noncanonical = format!("P{:02}.{:02}/helper", task.plan.get(), task.ordinal);
    let canonical_seq = insert_event(&board, plan, "worker_add", &canonical, None, author);
    let noncanonical_seq = insert_event(&board, plan, "worker_add", &noncanonical, None, author);

    let event_seqs = candidate_event_seqs(&board, worker_actor, "josh@laptop/grok/session1");

    assert!(event_seqs.contains(&canonical_seq));
    assert!(!event_seqs.contains(&noncanonical_seq));
}

fn actor_id(board: &LocalBoard, harness: &str) -> i64 {
    board
        .conn
        .query_row(
            "SELECT id FROM actors WHERE user='josh' AND host='laptop' AND harness=?1 AND session='session1'",
            [harness],
            |row| row.get(0),
        )
        .unwrap()
}

fn candidate_event_seqs(board: &LocalBoard, actor: i64, identity: &str) -> HashSet<i64> {
    let through = board.max_seq().unwrap().get() as i64;
    let candidates = super::pending_candidates();
    let mut statement = board
        .conn
        .prepare(&format!(
            "SELECT seq FROM ({candidates}) WHERE payload_kind='event'"
        ))
        .unwrap();
    statement
        .query_map(
            params![
                actor,
                identity,
                "josh",
                "laptop",
                identity.split('/').nth(1).unwrap(),
                "session1",
                1_800_000_000_i64,
                0_i64,
                through,
            ],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn insert_event(
    board: &LocalBoard,
    plan: PlanId,
    kind: &str,
    subject: &str,
    recipient: Option<&str>,
    author: i64,
) -> i64 {
    board
        .conn
        .execute(
            concat!(
                "INSERT INTO events(plan_id,kind,subject,to_whom,actor_id,summary,created_at) ",
                "VALUES(?1,?2,?3,?4,?5,'pending query regression',0)"
            ),
            params![plan.get() as i64, kind, subject, recipient, author],
        )
        .unwrap();
    board.conn.last_insert_rowid()
}
