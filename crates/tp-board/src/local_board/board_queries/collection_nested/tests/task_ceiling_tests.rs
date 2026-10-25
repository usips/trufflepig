use super::*;

#[test]
fn collection_tasks_paginate_completely_and_retain_moved_members() {
    let (_directory, mut board) = database();
    for ordinal in 1..=5 {
        task(&board.conn, ordinal, ordinal + 2);
        entry(&board.conn, ordinal + 10, ordinal + 2, "task");
    }
    let tx = board
        .reader
        .as_mut()
        .expect("read connection")
        .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
        .unwrap();
    let first = tasks(&tx, None, None, None, 2);
    assert_eq!(
        first
            .tasks
            .iter()
            .map(|task| task.id.ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(first.omitted, 3);
    assert_eq!(first.through, EventSeq::new(7));
    assert_eq!(first.ceiling.ordinal, 5);
    task(&board.conn, 6, 8);
    entry(&board.conn, 16, 8, "task");
    board
        .conn
        .execute("UPDATE tasks SET seq=8 WHERE ordinal=5", [])
        .unwrap();
    let second = tasks(
        &tx,
        first.next_after,
        Some(first.ceiling),
        Some(first.through),
        2,
    );
    assert_eq!(
        second
            .tasks
            .iter()
            .map(|task| task.id.ordinal)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert_eq!(second.omitted, 1);
    let third = tasks(
        &tx,
        second.next_after,
        Some(first.ceiling),
        Some(first.through),
        2,
    );
    assert_eq!(third.tasks[0].id.ordinal, 5);
    assert_eq!(third.omitted, 0);
    assert_eq!(third.next_after, None);
    tx.commit().unwrap();
    let current = tasks(
        board.reader.as_ref().expect("read connection"),
        None,
        Some(first.ceiling),
        Some(first.through),
        200,
    );
    assert_eq!(
        current
            .tasks
            .iter()
            .map(|task| task.id.ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    assert_eq!(current.tasks[4].seq, EventSeq::new(8));
    let refreshed = tasks(
        board.reader.as_ref().expect("read connection"),
        None,
        None,
        None,
        200,
    );
    assert_eq!(refreshed.tasks.len(), 6);
    assert_eq!(
        tasks_page(
            board.reader.as_ref().expect("read connection"),
            plan(1),
            Some(TaskId::new(plan(2), 1).unwrap()),
            Some(first.ceiling),
            None,
            1,
            TaskSelection::default(),
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
}

#[test]
fn collection_task_ceiling_keeps_empty_membership_and_validates_continuations() {
    let (_directory, board) = database();
    let empty = tasks(
        board.reader.as_ref().expect("read connection"),
        None,
        None,
        None,
        1,
    );
    assert_eq!(
        empty.ceiling,
        TaskCeiling {
            plan: plan(1),
            ordinal: 0
        }
    );
    task(&board.conn, 1, 3);
    entry(&board.conn, 3, 3, "task");
    let frozen = tasks(
        board.reader.as_ref().expect("read connection"),
        None,
        Some(empty.ceiling),
        Some(empty.through),
        1,
    );
    assert!(frozen.tasks.is_empty());
    assert_eq!(frozen.next_after, None);
    assert_eq!(
        tasks(
            board.reader.as_ref().expect("read connection"),
            None,
            None,
            None,
            1
        )
        .tasks
        .len(),
        1
    );
    let beyond = tasks(
        board.reader.as_ref().expect("read connection"),
        Some(TaskId::new(plan(1), 1).unwrap()),
        Some(empty.ceiling),
        Some(empty.through),
        1,
    );
    assert!(beyond.tasks.is_empty());
    assert_eq!(
        tasks_page(
            board.reader.as_ref().expect("read connection"),
            plan(1),
            None,
            None,
            Some(empty.through),
            1,
            TaskSelection::default(),
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        tasks_page(
            board.reader.as_ref().expect("read connection"),
            plan(1),
            Some(TaskId::new(plan(1), 1).unwrap()),
            None,
            None,
            1,
            TaskSelection::default(),
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        tasks_page(
            board.reader.as_ref().expect("read connection"),
            plan(1),
            None,
            Some(TaskCeiling {
                plan: plan(2),
                ordinal: 0
            }),
            None,
            1,
            TaskSelection::default(),
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
    assert_eq!(
        tasks_page(
            board.reader.as_ref().expect("read connection"),
            plan(1),
            None,
            Some(TaskCeiling {
                plan: plan(1),
                ordinal: i64::MAX as u64 + 1
            }),
            None,
            1,
            TaskSelection::default(),
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
}
