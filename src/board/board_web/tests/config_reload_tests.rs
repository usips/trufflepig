use super::*;

#[test]
fn config_reload_reaches_reader_ttl_and_attention_without_events() {
    let directory = crate::board::board_test_support::scratch("web-config-");
    let config_path = directory.path().join("board.toml");
    std::fs::write(&config_path, "user = 'owner'\nclaim_ttl_minutes = 120\n").unwrap();
    let defaults = BoardConfig::for_database(directory.path().join("board.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_source(config_path.clone(), defaults),
        directory.path().join("runtime"),
    )
    .unwrap();
    let config = store
        .config(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let created = read(
        &store,
        BoardOp::New {
            title: PlanTitle::new("Trial").unwrap(),
            body: PlanText::new("old\n").unwrap(),
            steward: None,
            repo_key: None,
        },
    );
    let BoardResult::Change(created) = created.result else {
        panic!("expected plan receipt")
    };
    let plan = created.plan.unwrap();
    let created = read(
        &store,
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Task").unwrap(),
            to: None,
            section: None,
        },
    );
    let BoardResult::Change(created) = created.result else {
        panic!("expected task receipt")
    };
    let task = created.task.unwrap();
    let actor = config.actor(Some("codex"), Some("worker")).unwrap();
    let proposal = {
        let mut slot = store.writer.lock().unwrap();
        let writer = slot.as_mut().unwrap();
        writer
            .handle(&BoardRequest::new(
                actor.clone(),
                BoardOp::ClaimTask {
                    task,
                    scope: Some(EntryText::new("scope").unwrap()),
                    resume: ClaimResume::No,
                    delegate: None,
                },
            ))
            .unwrap();
        writer
            .handle(&BoardRequest::new(
                actor,
                BoardOp::Propose {
                    base: crate::board::board_ids::PlanRevision::new(plan, 1).unwrap(),
                    body: PlanText::new("new\n").unwrap(),
                    summary: EntryText::new("proposal").unwrap(),
                    supersedes: None,
                },
            ))
            .unwrap()
    };
    let BoardResult::Change(proposal) = proposal.result else {
        panic!("expected proposal receipt")
    };
    rusqlite::Connection::open(&config.db_path)
        .unwrap()
        .execute("UPDATE claims SET last_active=last_active-120", [])
        .unwrap();
    let before = read(
        &store,
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
    );
    let BoardResult::Plan(view) = &before.result else {
        panic!("expected plan")
    };
    assert_eq!(view.claims.len(), 1);
    assert!(view.claims.iter().all(|claim| !claim.stale));
    let attention = || {
        serde_json::from_value(serde_json::json!({"op":"attention","scope":"all","limit":200}))
            .unwrap()
    };
    let original = serde_json::to_value(read(&store, attention())).unwrap();
    assert!(
        original["result"]["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == proposal.entry.to_string())
    );
    std::fs::write(&config_path, "user = 'reviewer'\nclaim_ttl_minutes = 1\n").unwrap();
    let refreshed = store
        .config
        .lock()
        .unwrap()
        .get(Instant::now() + Duration::from_secs(3))
        .unwrap();
    let changed = read(&store, attention());
    assert_eq!(changed.snapshot_seq, before.snapshot_seq);
    let changed = serde_json::to_value(changed).unwrap();
    assert_eq!(changed["result"]["data"]["actor"]["user"], "reviewer");
    assert!(
        !changed["result"]["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == proposal.entry.to_string())
    );
    let request = BoardRequest::new(
        refreshed.actor(Some("human"), Some("web")).unwrap(),
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
    );
    assert_stale_readers(&store.readers, &refreshed, &request, 4);
}
