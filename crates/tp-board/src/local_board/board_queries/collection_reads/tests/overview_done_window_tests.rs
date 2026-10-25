use super::*;
use crate::board_ids::TaskId;
use crate::board_protocol::ClaimResume;
use crate::board_vocabulary::{EntryText, TaskColumn};
use crate::local_board::board_queries::collection_nested;
use crate::local_board::board_writes::task_claims::claim_task;
use crate::local_board::board_writes::task_writes::move_task;
use serde_json::{Value, json};

fn move_seeded_task(board: &LocalBoard, ordinal: u64, column: TaskColumn, seq: u64, now: i64) {
    let mut ctx = context();
    let tx = board.conn.unchecked_transaction().unwrap();
    let card = crate::local_board::board_writes::task_writes::require_task(
        &tx,
        TaskId::new(plan(1), ordinal).unwrap(),
    )
    .unwrap();
    let reopening_done = card.column == TaskColumn::Done && column == TaskColumn::Todo;
    ctx.actor = if reopening_done {
        BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse("human").unwrap(),
            "owner",
        )
        .unwrap()
    } else {
        BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse("claude").unwrap(),
            "s2",
        )
        .unwrap()
    };
    ctx.actor_id = crate::local_board::ensure_actor(&tx, &ctx.actor, now).unwrap();
    ctx.seq = EventSeq::new(
        seq.max(
            crate::local_board::max_seq(&tx)
                .unwrap()
                .get()
                .saturating_add(1),
        ),
    );
    ctx.now = now;
    let task = TaskId::new(plan(1), ordinal).unwrap();
    if !reopening_done {
        let scope = EntryText::new("overview fixture ownership").unwrap();
        claim_task(&tx, &ctx, task, Some(&scope), ClaimResume::No, None).unwrap();
        ctx.seq = EventSeq::new(
            crate::local_board::max_seq(&tx)
                .unwrap()
                .get()
                .saturating_add(1),
        );
    }
    move_task(&tx, &ctx, task, column, None).unwrap();
    tx.commit().unwrap();
}

fn plan_overview_json(board: &LocalBoard) -> Value {
    let BoardResult::Overview(page) = overview(
        board.reader.as_ref().expect("read connection"),
        &context(),
        &ReadScope::All,
        None,
        false,
        None,
        1,
    )
    .unwrap()
    .result
    else {
        panic!("overview");
    };
    serde_json::to_value(&page.plans[0]).unwrap()
}

fn task_ids(tasks: &Value) -> Vec<Value> {
    tasks
        .as_array()
        .expect("task window")
        .iter()
        .map(|task| task["id"].clone())
        .collect()
}

fn expected_ids(ordinals: &[u64]) -> Vec<Value> {
    ordinals
        .iter()
        .map(|&ordinal| serde_json::to_value(TaskId::new(plan(1), ordinal).unwrap()).unwrap())
        .collect()
}

#[test]
fn overview_keeps_active_tasks_past_25_done() {
    let (_directory, board) = database();
    for ordinal in 1..=25 {
        seed_task(&board.conn, ordinal);
        move_seeded_task(&board, ordinal, TaskColumn::Done, ordinal + 2, 200);
    }
    for ordinal in 26..=28 {
        seed_task(&board.conn, ordinal);
    }

    let overview = plan_overview_json(&board);
    assert_eq!(task_ids(&overview["tasks"]), expected_ids(&[26, 27, 28]));
    assert_eq!(overview["tasks_omitted"], json!(0));
    assert_eq!(overview["done_count"], json!(25));
    assert_eq!(
        task_ids(&overview["recent_done"]),
        expected_ids(&[25, 24, 23, 22, 21])
    );
    for task in overview["tasks"].as_array().unwrap() {
        assert_eq!(task.get("done_at"), Some(&Value::Null));
    }
}

#[test]
fn recent_done_orders_by_completion() {
    let (_directory, board) = database();
    for ordinal in 1..=7 {
        seed_task(&board.conn, ordinal);
    }
    for (offset, ordinal) in [7, 2, 6, 1, 5, 3, 4].into_iter().enumerate() {
        move_seeded_task(
            &board,
            ordinal,
            TaskColumn::Done,
            offset as u64 + 3,
            1_000 - ordinal as i64,
        );
    }

    let overview = plan_overview_json(&board);
    assert_eq!(overview["done_count"], json!(7));
    assert_eq!(
        task_ids(&overview["recent_done"]),
        expected_ids(&[4, 3, 5, 1, 6])
    );
    let done_at = overview["recent_done"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["done_at"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        done_at,
        vec![json!(996), json!(997), json!(995), json!(999), json!(994)]
    );
    assert_eq!(overview["tasks"], json!([]));
    assert_eq!(overview["tasks_omitted"], json!(0));
}

#[test]
fn overview_counts_only_active_omissions() {
    let (_directory, board) = database();
    for ordinal in 1..=5 {
        seed_task(&board.conn, ordinal);
        move_seeded_task(&board, ordinal, TaskColumn::Done, ordinal + 2, 200);
    }
    for ordinal in 6..=26 {
        seed_task(&board.conn, ordinal);
        let column = ["todo", "doing", "review", "blocked"][(ordinal as usize - 6) % 4];
        board
            .conn
            .execute(
                "UPDATE tasks SET column_name=?1 WHERE plan_id=1 AND ordinal=?2",
                params![column, ordinal as i64],
            )
            .unwrap();
    }

    let overview = plan_overview_json(&board);
    assert_eq!(
        task_ids(&overview["tasks"]),
        expected_ids(&(6..=25).collect::<Vec<_>>())
    );
    assert_eq!(overview["tasks_omitted"], json!(1));
    assert_eq!(overview["done_count"], json!(5));
    assert_eq!(overview["task_ceiling"]["ordinal"], json!(26));
    for task in overview["tasks"].as_array().unwrap() {
        assert_eq!(task.get("done_at"), Some(&Value::Null));
    }
}

#[test]
fn task_done_at_tracks_completion_and_clears_when_reopened() {
    let (_directory, board) = database();
    seed_task(&board.conn, 1);
    move_seeded_task(&board, 1, TaskColumn::Review, 3, 250);
    let overview = plan_overview_json(&board);
    assert_eq!(overview["tasks"][0].get("done_at"), Some(&Value::Null));

    move_seeded_task(&board, 1, TaskColumn::Done, 4, 600);
    let overview = plan_overview_json(&board);
    assert_eq!(overview["recent_done"][0]["done_at"], json!(600));
    let done_seq: i64 = board
        .conn
        .query_row(
            "SELECT seq FROM tasks WHERE plan_id=1 AND ordinal=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(overview["recent_done"][0]["seq"], json!(done_seq));

    move_seeded_task(&board, 1, TaskColumn::Todo, 5, 650);
    let overview = plan_overview_json(&board);
    assert_eq!(overview["tasks"][0].get("done_at"), Some(&Value::Null));
    assert_eq!(overview["recent_done"], json!([]));
    assert_eq!(overview["done_count"], json!(0));

    move_seeded_task(&board, 1, TaskColumn::Done, 6, 900);
    let page =
        collection_nested::task_window(&board.conn, plan(1), None, None, None, 20, None).unwrap();
    let tasks = serde_json::to_value(&page.tasks).unwrap();
    assert_eq!(tasks[0]["done_at"], json!(900));
    let tasks =
        crate::local_board::board_writes::task_claims::read_tasks(&board.conn, plan(1)).unwrap();
    let tasks = serde_json::to_value(tasks).unwrap();
    assert_eq!(tasks[0]["done_at"], json!(900));
}

#[test]
fn recent_done_breaks_sequence_ties_by_ordinal() {
    let (_directory, board) = database();
    seed_task(&board.conn, 1);
    seed_task(&board.conn, 2);
    move_seeded_task(&board, 1, TaskColumn::Done, 3, 600);
    let tied_seq: i64 = board
        .conn
        .query_row(
            "SELECT seq FROM tasks WHERE plan_id=1 AND ordinal=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE tasks SET column_name='done',seq=?1 WHERE ordinal=2",
            [tied_seq],
        )
        .unwrap();

    let overview = plan_overview_json(&board);
    assert_eq!(task_ids(&overview["recent_done"]), expected_ids(&[2, 1]));
}
