use super::*;
use crate::board_protocol::{WorkerAddress, WorkerEndReason, WorkerHandle, WorkerRole};

fn register_worker(
    conn: &mut Connection,
    manager: &BoardActor,
    now: i64,
    task: TaskId,
    handle: &str,
) -> WorkerAddress {
    let handle = WorkerHandle::parse(handle).unwrap();
    write(conn, manager, now, |tx, ctx| {
        add_worker(
            tx,
            ctx,
            task,
            &handle,
            WorkerRole::Code,
            "worker-model",
            Some("high"),
            &EntryText::new("implement the assigned files").unwrap(),
            &["src/worker.rs".to_owned()],
        )
    })
    .unwrap();
    WorkerAddress::new(task, handle).unwrap()
}

fn bind(
    conn: &mut Connection,
    actor: &BoardActor,
    now: i64,
    address: &WorkerAddress,
    model: &str,
    effort: Option<&str>,
) -> Result<(), BoardError> {
    write(conn, actor, now, |tx, ctx| {
        bind_worker(tx, ctx, address, model, effort)?;
        Ok(BoardReply::new("local", BoardResult::Cursor(ctx.seq)))
    })
    .map(|_| ())
}

fn create_card(conn: &mut Connection, actor: &BoardActor, now: i64, title: &str) -> TaskId {
    let reply = write(conn, actor, now, |tx, ctx| {
        create_task(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &PlanTitle::new(title).unwrap(),
            None,
            None,
        )
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected task creation change")
    };
    change.task.unwrap()
}

fn claim_entry(conn: &Connection, task: TaskId) -> EntryId {
    EntryId::new(
        conn.query_row(
            "SELECT entry_id FROM claims WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
            rusqlite::params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row_number(row, 0),
        )
        .unwrap(),
    )
    .unwrap()
}

fn grant_assign_to_actor(conn: &Connection, actor: &BoardActor) {
    let grantor = BoardActor::new(
        "josh",
        "test-host",
        HarnessLabel::parse("human").unwrap(),
        "worker-grant-fixture",
    )
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO actors(user,host,harness,session) VALUES(?1,?2,?3,?4)",
        rusqlite::params![
            grantor.user,
            grantor.host,
            grantor.harness.as_str(),
            grantor.session
        ],
    )
    .unwrap();
    let grantor_id: i64 = conn
        .query_row(
            "SELECT id FROM actors WHERE user=?1 AND host=?2 AND harness=?3 AND session=?4",
            rusqlite::params![
                grantor.user,
                grantor.host,
                grantor.harness.as_str(),
                grantor.session
            ],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO agent_sessions(actor_id,first_seen,last_seen) VALUES(?1,100,100) ON CONFLICT(actor_id) DO NOTHING",
        [grantor_id],
    )
    .unwrap();
    let seq: i64 = conn
        .query_row(
            "SELECT max(max_entry,max_event) FROM (SELECT COALESCE(MAX(seq),0) AS max_entry FROM entries), (SELECT COALESCE(MAX(seq),0) AS max_event FROM events)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let summary = format!("assign fixture for {}", actor.identity());
    let entry: i64 = conn
        .query_row(
            "INSERT INTO entries(plan_id,kind,body,to_whom,actor_id,seq,created_at,authority_basis) VALUES(1,'grant',?1,?2,?3,?4,100,'owner') RETURNING id",
            rusqlite::params![summary, actor.identity(), grantor_id, seq + 1],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO events(seq,plan_id,kind,subject,to_whom,actor_id,summary,created_at,authority_basis) VALUES(?1,1,'grant',?2,?3,?4,?5,100,'owner')",
        rusqlite::params![seq + 1, format!("E{entry}"), actor.identity(), grantor_id, summary],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO authority_grants(entry_id,plan_id,owner_user,roles_json,grantee,granted_at,expires_at,revoked_by_entry,source) VALUES(?1,1,'josh','[\"assign\"]',?2,100,NULL,NULL,'owner')",
        rusqlite::params![entry, actor.identity()],
    )
    .unwrap();
}

fn write_post(conn: &mut Connection, actor: &BoardActor, now: i64, task: TaskId) -> EntryId {
    let mut entry_id = None;
    write(conn, actor, now, |tx, ctx| {
        let entry = insert_entry(
            tx,
            ctx,
            &EntryDraft {
                plan_id: Some(task.plan),
                kind: EntryKind::Note,
                body: "worker post".to_owned(),
                to_whom: None,
                supersedes: None,
                repo_key: None,
                state: None,
            },
        )?;
        attach_worker_post(tx, ctx, task, entry)?;
        entry_id = Some(entry);
        Ok(BoardReply::new("local", BoardResult::Cursor(ctx.seq)))
    })
    .unwrap();
    entry_id.unwrap()
}

#[test]
fn unrelated_actor_cannot_register_worker_on_a_live_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let task = carved_task(&mut conn, &holder, 1000, "worker lane");
    let handle = WorkerHandle::parse("helper").unwrap();
    let error = write(
        &mut conn,
        &actor("other", "muse", "intruder"),
        1010,
        |tx, ctx| {
            add_worker(
                tx,
                ctx,
                task,
                &handle,
                WorkerRole::Code,
                "worker-model",
                Some("high"),
                &EntryText::new("unauthorized work").unwrap(),
                &["src/worker.rs".to_owned()],
            )
        },
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("invalid_actor:"), "{error}");
    let count: i64 = conn
        .query_row("SELECT count(*) FROM claim_workers", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn bound_worker_cannot_self_add_or_take_over_its_lead_task_without_authority() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let worker_actor = actor("josh", "muse", "worker-session");
    let task = carved_task(&mut conn, &holder, 1000, "worker lane");
    let address = register_worker(&mut conn, &holder, 1001, task, "helper");
    bind(
        &mut conn,
        &worker_actor,
        1002,
        &address,
        "worker-model",
        Some("high"),
    )
    .unwrap();

    let handle = WorkerHandle::parse("second").unwrap();
    let add_error = write(&mut conn, &worker_actor, 1010, |tx, ctx| {
        add_worker(
            tx,
            ctx,
            task,
            &handle,
            WorkerRole::Test,
            "worker-model",
            None,
            &EntryText::new("another lane").unwrap(),
            &["tests/worker.rs".to_owned()],
        )
    })
    .unwrap_err();
    assert!(add_error.to_string().starts_with("invalid_actor:"));

    let claim_error = claim(&mut conn, &worker_actor, 1123, task, "worker takeover").unwrap_err();
    assert!(claim_error.to_string().starts_with("invalid_actor:"));
}

#[test]
fn task_lifetime_handle_cannot_be_reused_after_takeover() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let old_holder = actor("josh", "codex", "old");
    let new_holder = actor("josh", "muse", "new");
    let task = carved_task(&mut conn, &old_holder, 1000, "original lane");
    let address = register_worker(&mut conn, &old_holder, 1001, task, "helper");

    claim(&mut conn, &new_holder, 1121, task, "replacement lane").unwrap();
    let old_worker = worker_for_address(&conn, &address).unwrap().unwrap();
    assert_eq!(old_worker.record.state, Some(WorkerEndReason::ClaimEnded));
    assert_eq!(old_worker.ended_at, Some(1121));

    let handle = WorkerHandle::parse("helper").unwrap();
    let error = write(&mut conn, &new_holder, 1122, |tx, ctx| {
        add_worker(
            tx,
            ctx,
            task,
            &handle,
            WorkerRole::Test,
            "worker-model",
            None,
            &EntryText::new("reused handle").unwrap(),
            &["tests/worker.rs".to_owned()],
        )
    })
    .unwrap_err();
    assert_eq!(
        error.code,
        crate::board_protocol::BoardErrorCode::InvalidWorker,
        "{error}"
    );
}

#[test]
fn binding_is_idempotent_for_its_owner_and_one_actor_cannot_bind_twice() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let worker_actor = actor("josh", "muse", "worker-session");
    let task = carved_task(&mut conn, &holder, 1000, "worker lane");
    let first = register_worker(&mut conn, &holder, 1001, task, "first");
    let second = register_worker(&mut conn, &holder, 1002, task, "second");

    bind(
        &mut conn,
        &worker_actor,
        1003,
        &first,
        "worker-model",
        Some("high"),
    )
    .unwrap();
    bind(
        &mut conn,
        &worker_actor,
        1004,
        &first,
        "worker-model",
        Some("high"),
    )
    .unwrap();
    let error = bind(
        &mut conn,
        &worker_actor,
        1005,
        &second,
        "worker-model",
        Some("high"),
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("invalid_actor:"), "{error}");
    let worker_actor_id: i64 = conn
        .query_row(
            "SELECT id FROM actors WHERE user='josh' AND host='test-host' AND harness='muse' AND session='worker-session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let active = active_worker_binding(&conn, worker_actor_id, task)
        .unwrap()
        .unwrap();
    assert_eq!(active.address, first);
    let bound: i64 = conn
        .query_row(
            "SELECT count(*) FROM claim_workers WHERE actor_id IS NOT NULL AND ended_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bound, 1);
}

#[test]
fn binding_requires_the_declared_model_and_effort() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let worker_actor = actor("josh", "muse", "worker-session");
    let task = carved_task(&mut conn, &holder, 1000, "worker lane");
    let address = register_worker(&mut conn, &holder, 1001, task, "helper");

    let wrong_model = bind(
        &mut conn,
        &worker_actor,
        1002,
        &address,
        "other-model",
        Some("high"),
    )
    .unwrap_err();
    assert!(wrong_model.to_string().starts_with("invalid_actor:"));
    let wrong_effort = bind(
        &mut conn,
        &worker_actor,
        1003,
        &address,
        "worker-model",
        Some("low"),
    )
    .unwrap_err();
    assert!(wrong_effort.to_string().starts_with("invalid_actor:"));
    bind(
        &mut conn,
        &worker_actor,
        1004,
        &address,
        "worker-model",
        Some("high"),
    )
    .unwrap();
}

#[test]
fn worker_posts_refresh_only_the_bound_worker_and_its_parent_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let worker_actor = actor("josh", "muse", "worker-session");
    let parent_task = carved_task(&mut conn, &holder, 1000, "worker lane");
    let address = register_worker(&mut conn, &holder, 1001, parent_task, "helper");
    bind(
        &mut conn,
        &worker_actor,
        1002,
        &address,
        "worker-model",
        Some("high"),
    )
    .unwrap();
    grant_assign_to_actor(&conn, &worker_actor);
    let parent_entry = claim_entry(&conn, parent_task);
    let worker_id = worker_for_address(&conn, &address).unwrap().unwrap().id;

    let other_task = create_card(&mut conn, &worker_actor, 1010, "separate lead card");
    claim(
        &mut conn,
        &worker_actor,
        1011,
        other_task,
        "ordinary lead work",
    )
    .unwrap();
    let other_entry = claim_entry(&conn, other_task);
    let cross_post = write_post(&mut conn, &worker_actor, 1020, other_task);
    let cross_worker_id: Option<i64> = conn
        .query_row(
            "SELECT worker_id FROM entries WHERE id=?1",
            [sql_number(cross_post.get())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cross_worker_id, None);
    let parent_after_cross: (i64, i64) = conn
        .query_row(
            "SELECT w.last_active,c.last_active FROM claim_workers w JOIN claims c ON c.id=w.claim_id WHERE w.id=?1",
            [worker_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(parent_after_cross, (1002, 1002));

    let parent_post = write_post(&mut conn, &worker_actor, 1021, parent_task);
    let attached_worker: Option<i64> = conn
        .query_row(
            "SELECT worker_id FROM entries WHERE id=?1",
            [sql_number(parent_post.get())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(attached_worker, Some(worker_id));
    let parent_after_local: (i64, i64) = conn
        .query_row(
            "SELECT w.last_active,c.last_active FROM claim_workers w JOIN claims c ON c.id=w.claim_id WHERE w.id=?1",
            [worker_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(parent_after_local, (1021, 1021));
    let other_last_active: i64 = conn
        .query_row(
            "SELECT last_active FROM claims WHERE entry_id=?1",
            [sql_number(other_entry.get())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(other_last_active, 1011);
    assert_eq!(
        worker_for_address(&conn, &address)
            .unwrap()
            .unwrap()
            .claim_entry,
        parent_entry
    );
}

#[test]
fn ending_parent_claim_ends_all_worker_nodes() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let task = carved_task(&mut conn, &holder, 1000, "worker lane");
    let address = register_worker(&mut conn, &holder, 1001, task, "helper");
    write(&mut conn, &holder, 1010, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Todo, None)
    })
    .unwrap();

    let worker = worker_for_address(&conn, &address).unwrap().unwrap();
    assert_eq!(worker.record.state, Some(WorkerEndReason::ClaimEnded));
    assert_eq!(worker.ended_at, Some(1010));
}

#[test]
fn worker_end_requires_manager_authority_and_preserves_its_note() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let worker_actor = actor("josh", "muse", "worker-session");
    let task = carved_task(&mut conn, &holder, 1000, "worker lane");
    let address = register_worker(&mut conn, &holder, 1001, task, "helper");
    bind(
        &mut conn,
        &worker_actor,
        1002,
        &address,
        "worker-model",
        Some("high"),
    )
    .unwrap();

    let worker_error = write(&mut conn, &worker_actor, 1003, |tx, ctx| {
        end_worker(tx, ctx, &address, WorkerEndReason::Failed, None)
    })
    .unwrap_err();
    assert!(worker_error.to_string().starts_with("invalid_actor:"));

    write(&mut conn, &holder, 1004, |tx, ctx| {
        end_worker(
            tx,
            ctx,
            &address,
            WorkerEndReason::Failed,
            Some(&EntryText::new("the focused check failed").unwrap()),
        )
    })
    .unwrap();
    let worker = worker_for_address(&conn, &address).unwrap().unwrap();
    assert_eq!(worker.record.state, Some(WorkerEndReason::Failed));
    assert_eq!(
        worker.record.note.as_ref().map(EntryText::as_str),
        Some("the focused check failed")
    );
    assert_eq!(worker.ended_at, Some(1004));
}

#[test]
fn worker_bye_stops_only_its_node_and_ends_its_lead_claims() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let worker_actor = actor("josh", "muse", "worker-session");
    let sibling_actor = actor("josh", "claude", "sibling-session");
    let parent_task = carved_task(&mut conn, &holder, 1000, "worker lane");
    let address = register_worker(&mut conn, &holder, 1001, parent_task, "helper");
    let sibling_address = register_worker(&mut conn, &holder, 1002, parent_task, "sibling");
    bind(
        &mut conn,
        &worker_actor,
        1003,
        &address,
        "worker-model",
        Some("high"),
    )
    .unwrap();
    bind(
        &mut conn,
        &sibling_actor,
        1004,
        &sibling_address,
        "worker-model",
        Some("high"),
    )
    .unwrap();
    grant_assign_to_actor(&conn, &worker_actor);

    let lead_task = create_card(&mut conn, &worker_actor, 1005, "worker-owned lead card");
    claim(&mut conn, &worker_actor, 1006, lead_task, "lead card work").unwrap();
    let review_task = create_card(&mut conn, &worker_actor, 1007, "review card");
    let done_task = create_card(&mut conn, &worker_actor, 1008, "done card");
    conn.execute(
        "UPDATE tasks SET column_name='review' WHERE plan_id=?1 AND ordinal=?2",
        rusqlite::params![
            sql_number(review_task.plan.get()),
            sql_number(review_task.ordinal)
        ],
    )
    .unwrap();
    conn.execute(
        "UPDATE tasks SET column_name='done' WHERE plan_id=?1 AND ordinal=?2",
        rusqlite::params![
            sql_number(done_task.plan.get()),
            sql_number(done_task.ordinal)
        ],
    )
    .unwrap();

    let first = write(&mut conn, &worker_actor, 1010, |tx, ctx| bye(tx, ctx)).unwrap();
    let BoardResult::Bye(first_bye) = first.result else {
        panic!("expected bye result")
    };
    assert_eq!(first_bye.outcome, crate::board_protocol::ByeOutcome::Ended);
    let worker = worker_for_address(&conn, &address).unwrap().unwrap();
    assert_eq!(worker.record.state, Some(WorkerEndReason::Stopped));
    assert_eq!(worker.ended_at, Some(1010));
    let sibling = worker_for_address(&conn, &sibling_address)
        .unwrap()
        .unwrap();
    assert_eq!(sibling.record.state, None);
    assert_eq!(sibling.ended_at, None);
    let lead_history =
        read_claims_window(&conn, lead_task.plan, i64::MIN, i64::MAX, 1010, 120).unwrap();
    let ended_lead = lead_history
        .iter()
        .find(|claim| claim.task == lead_task)
        .unwrap();
    assert_eq!(ended_lead.end_reason, Some(ClaimEndReason::SessionEnded));
    assert_eq!(ended_lead.ended_at, Some(1010));
    let tasks = read_tasks(&conn, lead_task.plan).unwrap();
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == lead_task)
            .unwrap()
            .column,
        TaskColumn::Todo
    );
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == review_task)
            .unwrap()
            .column,
        TaskColumn::Review
    );
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == done_task)
            .unwrap()
            .column,
        TaskColumn::Done
    );
    let parent_history =
        read_claims_window(&conn, parent_task.plan, i64::MIN, i64::MAX, 1010, 120).unwrap();
    assert!(parent_history[0].ended_at.is_none());
    assert_eq!(
        read_tasks(&conn, parent_task.plan)
            .unwrap()
            .iter()
            .find(|task| task.id == parent_task)
            .unwrap()
            .column,
        TaskColumn::Doing
    );

    let first_bye_events: i64 = conn
        .query_row("SELECT count(*) FROM events WHERE kind='bye'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(first_bye_events, 1);
    let second = write(&mut conn, &worker_actor, 1011, |tx, ctx| bye(tx, ctx)).unwrap();
    let BoardResult::Bye(second_bye) = second.result else {
        panic!("expected idempotent bye result")
    };
    assert_eq!(
        second_bye.outcome,
        crate::board_protocol::ByeOutcome::AlreadyEnded
    );
    assert_eq!(
        (second_bye.entry, second_bye.seq),
        (first_bye.entry, first_bye.seq)
    );
    let second_bye_events: i64 = conn
        .query_row("SELECT count(*) FROM events WHERE kind='bye'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(second_bye_events, 1);
    let session_ended_at: Option<i64> = conn
        .query_row(
            "SELECT ended_at FROM agent_sessions s JOIN actors a ON a.id=s.actor_id WHERE a.user='josh' AND a.host='test-host' AND a.harness='muse' AND a.session='worker-session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(session_ended_at, Some(1010));
}

#[test]
fn lead_bye_ends_its_claim_and_preserves_review_and_done_cards() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let lead = actor("josh", "codex", "lead-session");
    let doing_task = create_card(&mut conn, &lead, 1000, "lead work");
    claim(&mut conn, &lead, 1001, doing_task, "lead card work").unwrap();
    let review_task = create_card(&mut conn, &lead, 1002, "review card");
    let done_task = create_card(&mut conn, &lead, 1003, "done card");
    conn.execute(
        "UPDATE tasks SET column_name='review' WHERE plan_id=?1 AND ordinal=?2",
        rusqlite::params![
            sql_number(review_task.plan.get()),
            sql_number(review_task.ordinal)
        ],
    )
    .unwrap();
    conn.execute(
        "UPDATE tasks SET column_name='done' WHERE plan_id=?1 AND ordinal=?2",
        rusqlite::params![
            sql_number(done_task.plan.get()),
            sql_number(done_task.ordinal)
        ],
    )
    .unwrap();

    write(&mut conn, &lead, 1010, |tx, ctx| bye(tx, ctx)).unwrap();

    let history =
        read_claims_window(&conn, doing_task.plan, i64::MIN, i64::MAX, 1010, 120).unwrap();
    let ended = history
        .iter()
        .find(|claim| claim.task == doing_task)
        .unwrap();
    assert_eq!(ended.end_reason, Some(ClaimEndReason::SessionEnded));
    assert_eq!(ended.ended_at, Some(1010));
    let tasks = read_tasks(&conn, doing_task.plan).unwrap();
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == doing_task)
            .unwrap()
            .column,
        TaskColumn::Todo
    );
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == review_task)
            .unwrap()
            .column,
        TaskColumn::Review
    );
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == done_task)
            .unwrap()
            .column,
        TaskColumn::Done
    );
}

#[test]
fn holder_reclaiming_with_new_scope_keeps_its_bound_worker_active() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let worker_actor = actor("josh", "muse", "worker-session");
    let task = carved_task(&mut conn, &holder, 1000, "worker lane");
    let address = register_worker(&mut conn, &holder, 1001, task, "helper");
    bind(
        &mut conn,
        &worker_actor,
        1002,
        &address,
        "worker-model",
        Some("high"),
    )
    .unwrap();
    let lease = claim_entry(&conn, task);

    claim(&mut conn, &holder, 1010, task, "reshaped lane").unwrap();

    assert_eq!(claim_entry(&conn, task), lease, "same lease after re-claim");
    let scope: String = conn
        .query_row(
            "SELECT scope FROM claims WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
            rusqlite::params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(scope, "reshaped lane");
    let worker = worker_for_address(&conn, &address).unwrap().unwrap();
    assert_eq!(worker.ended_at, None, "re-claim must not end the worker");
    assert_eq!(worker.record.state, None);
}

fn register_with_files(
    conn: &mut Connection,
    holder: &BoardActor,
    task: TaskId,
    handle: &str,
    files: &[String],
) -> Result<BoardReply, BoardError> {
    let handle = WorkerHandle::parse(handle).unwrap();
    write(conn, holder, 1001, |tx, ctx| {
        add_worker(
            tx,
            ctx,
            task,
            &handle,
            WorkerRole::Code,
            "worker-model",
            Some("high"),
            &EntryText::new("implement the assigned files").unwrap(),
            files,
        )
    })
}

#[test]
fn worker_file_limits_are_64_paths_of_1024_bytes_at_registration() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "lead");
    let task = carved_task(&mut conn, &holder, 1000, "file lane");
    let distinct = |count: usize| -> Vec<String> {
        (0..count)
            .map(|index| format!("src/file{index}.rs"))
            .collect()
    };
    let too_many = register_with_files(&mut conn, &holder, task, "many", &distinct(65));
    assert!(
        too_many
            .unwrap_err()
            .to_string()
            .starts_with("invalid_options:")
    );
    let too_long = register_with_files(&mut conn, &holder, task, "long", &["a".repeat(1025)]);
    assert!(
        too_long
            .unwrap_err()
            .to_string()
            .starts_with("invalid_options:")
    );
    register_with_files(&mut conn, &holder, task, "limits", &distinct(64)).unwrap();
    register_with_files(&mut conn, &holder, task, "exact", &["b".repeat(1024)]).unwrap();
}
