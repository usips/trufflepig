use super::*;

#[test]
fn simultaneous_carves_allocate_distinct_ordinals_and_sections() {
    let database = ClaimDatabase::new();
    let first = database.connect();
    let second = database.connect();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .enumerate()
        .map(|(index, mut conn)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                carved_task(
                    &mut conn,
                    &actor("josh", "codex", &format!("session-{index}")),
                    1000,
                    "exclusive carve",
                )
            })
        })
        .collect();
    let mut ordinals: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().ordinal)
        .collect();
    ordinals.sort_unstable();
    assert_eq!(ordinals, [1, 2]);
    let conn = database.connect();
    let tasks = read_tasks(&conn, PlanId::new(1).unwrap()).unwrap();
    assert!(
        tasks
            .iter()
            .all(|task| task.section.as_deref() == Some("Claims")
                && task.column == TaskColumn::Doing)
    );
    assert_eq!(
        read_claims_window(
            &conn,
            PlanId::new(1).unwrap(),
            i64::MIN,
            i64::MAX,
            1000,
            120
        )
        .unwrap()
        .len(),
        2
    );
}

#[test]
fn simultaneous_claimants_leave_one_active_lease() {
    let database = ClaimDatabase::new();
    let mut setup = database.connect();
    let task = carved_task(
        &mut setup,
        &actor("josh", "codex", "original"),
        1000,
        "initial",
    );
    let first = database.connect();
    let second = database.connect();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .enumerate()
        .map(|(index, mut conn)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                claim(
                    &mut conn,
                    &actor("josh", "muse", &format!("new-{index}")),
                    1121,
                    task,
                    "replacement",
                )
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| error.to_string().starts_with("claim_conflict:"))
    );
    let live: i64 = setup
        .query_row(
            "SELECT count(*) FROM claims WHERE ended_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live, 1);
}

#[test]
fn failed_carve_rolls_back_task_ordinal_and_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    conn.execute_batch("CREATE TRIGGER refuse_claim_event BEFORE INSERT ON events WHEN NEW.kind='claim' BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    let result = write(
        &mut conn,
        &actor("josh", "codex", "one"),
        1000,
        |tx, ctx| {
            carve_claim(
                tx,
                ctx,
                PlanId::new(1).unwrap(),
                &PlanTitle::new("Atomic").unwrap(),
                &EntryText::new("scope").unwrap(),
                Some("Heading"),
            )
        },
    );
    assert!(result.is_err());
    let ordinal: i64 = conn
        .query_row("SELECT next_task FROM plans WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ordinal, 0);
    assert!(
        read_tasks(&conn, PlanId::new(1).unwrap())
            .unwrap()
            .is_empty()
    );
    assert!(
        read_claims_window(
            &conn,
            PlanId::new(1).unwrap(),
            i64::MIN,
            i64::MAX,
            1000,
            120
        )
        .unwrap()
        .is_empty()
    );
}
