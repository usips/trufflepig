use super::*;

fn fixture() -> (tempfile::TempDir, Store, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let store = Store::open(&root, &directory.path().join("cache")).unwrap();
    let conn = Connection::open(directory.path().join("diagnostics.sqlite3")).unwrap();
    create_schema(&conn).unwrap();
    (directory, store, conn)
}

#[test]
fn session_baseline_survives_live_content_eviction_and_restart() {
    let (directory, mut store, conn) = fixture();
    std::fs::write(
        store.root.join("a.rs"),
        "fn original_name() { let source_body_secret = 1; }",
    )
    .unwrap();
    let started = start(&conn, &mut store).unwrap();
    let id = started["session"].as_str().unwrap();
    let baseline: String = conn
        .query_row(
            "SELECT baseline FROM diagnostic_sessions WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!baseline.contains("source_body_secret"));
    assert!(!baseline.contains("original_name"));
    std::fs::write(
        store.root.join("a.rs"),
        "fn original_name() { let replacement = 2; }",
    )
    .unwrap();
    store.index().unwrap();
    let count: i64 = store
        .conn
        .query_row("SELECT count(*) FROM contents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    drop(conn);
    let conn = Connection::open(directory.path().join("diagnostics.sqlite3")).unwrap();
    create_schema(&conn).unwrap();
    let report = end(&conn, &mut store, id).unwrap();
    assert_eq!(report["repository"], crate::store::encode_path(&store.root));
    assert_eq!(report["changes"][0]["category"], "modified");
    assert!(report["changes"][0]["before_revision"].is_string());
    assert_eq!(
        report["changes"][0]["occurrences"]["changes"][0]["correspondence"],
        "modified"
    );
    assert_eq!(baseline_bytes(&conn).unwrap(), 0);
    assert_eq!(
        audit(&conn, Some(id)).unwrap()["sessions"][0]["status"],
        "ended"
    );
}

#[test]
fn session_overlap_is_symmetric_and_forgotten_sessions_are_invalid() {
    let (_directory, mut store, conn) = fixture();
    let first = start(&conn, &mut store).unwrap();
    let second = start(&conn, &mut store).unwrap();
    assert_eq!(second["overlapping_observation"], true);
    let report = end(&conn, &mut store, first["session"].as_str().unwrap()).unwrap();
    assert_eq!(report["overlapping_observation"], true);
    assert_eq!(report["ownership"], "observational_only");
    forget(&conn).unwrap();
    assert!(end(&conn, &mut store, second["session"].as_str().unwrap()).is_err());
    assert!(
        audit(&conn, None).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn session_net_changes_distinguish_ignored_deleted_and_added_paths() {
    let (_directory, mut store, conn) = fixture();
    std::fs::write(store.root.join("ignored.rs"), "fn a() {}").unwrap();
    std::fs::write(store.root.join("deleted.rs"), "fn b() {}").unwrap();
    let started = start(&conn, &mut store).unwrap();
    std::fs::write(store.root.join(".ignore"), "ignored.rs\n").unwrap();
    std::fs::remove_file(store.root.join("deleted.rs")).unwrap();
    std::fs::write(store.root.join("added.rs"), "fn c() {}").unwrap();
    let report = end(&conn, &mut store, started["session"].as_str().unwrap()).unwrap();
    let changes = report["changes"].as_array().unwrap();
    let category = |path: &str| {
        changes
            .iter()
            .find(|change| change["path"] == path)
            .unwrap()["category"]
            .as_str()
            .unwrap()
    };
    assert_eq!(category("ignored.rs"), "ignored_or_excluded");
    assert_eq!(category("deleted.rs"), "deleted");
    assert_eq!(category("added.rs"), "addition_without_preimage");
}

#[test]
fn session_expiry_releases_baseline_and_remains_incomplete() {
    let (_directory, mut store, conn) = fixture();
    let started = start(&conn, &mut store).unwrap();
    let id = started["session"].as_str().unwrap();
    conn.execute(
        "UPDATE diagnostic_sessions SET started=?1 WHERE id=?2",
        params![now() - RETENTION_SECONDS - 1, id],
    )
    .unwrap();
    let report = audit(&conn, Some(id)).unwrap();
    assert_eq!(report["sessions"][0]["status"], "expired_incomplete");
    assert_eq!(baseline_bytes(&conn).unwrap(), 0);
    assert!(end(&conn, &mut store, id).is_err());
}

#[test]
fn session_limits_reject_additional_baselines() {
    let (_directory, mut store, conn) = fixture();
    for index in 0..MAX_OPEN_SESSIONS {
        conn.execute(
            "INSERT INTO diagnostic_sessions(id,started,status,baseline) VALUES(?1,?2,'open','{}')",
            params![index.to_string(), now()],
        )
        .unwrap();
    }
    assert!(
        start(&conn, &mut store)
            .unwrap_err()
            .to_string()
            .contains("50")
    );
    assert!(session_snapshot::capture(&mut store, 0).is_err());
}

#[test]
fn session_duplicate_declarations_remain_occurrences() {
    let (_directory, mut store, conn) = fixture();
    std::fs::write(
        store.root.join("a.rs"),
        "fn duplicate() {}\nfn duplicate() {}\n",
    )
    .unwrap();
    let started = start(&conn, &mut store).unwrap();
    let id = started["session"].as_str().unwrap();
    let baseline: String = conn
        .query_row(
            "SELECT baseline FROM diagnostic_sessions WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    let baseline: Value = serde_json::from_str(&baseline).unwrap();
    let facts = baseline["files"]["a.rs"]["facts"].as_array().unwrap();
    let declarations: Vec<_> = facts
        .iter()
        .filter(|fact| fact["category"] == "declaration")
        .collect();
    let duplicates: Vec<_> = declarations
        .iter()
        .filter(|fact| {
            declarations
                .iter()
                .filter(|other| other["key"] == fact["key"])
                .count()
                == 2
        })
        .collect();
    assert_eq!(duplicates.len(), 2);
    assert_ne!(
        duplicates[0]["span"]["start"],
        duplicates[1]["span"]["start"]
    );
    let key = duplicates[0]["key"].clone();
    std::fs::write(store.root.join("a.rs"), "fn duplicate() {}\n").unwrap();
    let report = end(&conn, &mut store, id).unwrap();
    let changes = report["changes"][0]["occurrences"]["changes"]
        .as_array()
        .unwrap();
    let deleted: Vec<_> = changes
        .iter()
        .filter(|change| change["before"]["key"] == key && change["correspondence"] == "deleted")
        .collect();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0]["deletion_proven"], true);
}

#[test]
fn session_reverted_edit_is_only_a_publication_observation() {
    let (_directory, mut store, conn) = fixture();
    std::fs::write(store.root.join("a.rs"), "fn original() {}\n").unwrap();
    let started = start(&conn, &mut store).unwrap();
    std::fs::write(store.root.join("a.rs"), "fn temporary() {}\n").unwrap();
    store.index().unwrap();
    std::fs::write(store.root.join("a.rs"), "fn original() {}\n").unwrap();
    let report = end(&conn, &mut store, started["session"].as_str().unwrap()).unwrap();
    assert!(report["changes"].as_array().unwrap().is_empty());
    assert_eq!(
        report["publication_observations"]["changes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(report["publication_observations"]["complete"], true);
}

#[test]
fn session_incomplete_extraction_cannot_prove_symbol_deletion() {
    let (_directory, mut store, conn) = fixture();
    std::fs::write(store.root.join("a.rs"), "fn original() {}\n").unwrap();
    let started = start(&conn, &mut store).unwrap();
    std::fs::write(store.root.join("a.rs"), "fn broken(\n").unwrap();
    let report = end(&conn, &mut store, started["session"].as_str().unwrap()).unwrap();
    let change = &report["changes"][0];
    assert_ne!(change["after_status"], "complete");
    assert!(
        change["occurrences"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["deletion_proven"] == false)
    );
}

#[test]
fn session_follows_only_unique_exact_content_renames() {
    let (_directory, mut store, conn) = fixture();
    std::fs::write(store.root.join("unique.rs"), "fn unique() {}\n").unwrap();
    std::fs::write(store.root.join("duplicate.rs"), "fn duplicate() {}\n").unwrap();
    std::fs::write(store.root.join("copy.rs"), "fn duplicate() {}\n").unwrap();
    let started = start(&conn, &mut store).unwrap();
    std::fs::rename(store.root.join("unique.rs"), store.root.join("moved.rs")).unwrap();
    std::fs::rename(
        store.root.join("duplicate.rs"),
        store.root.join("ambiguous.rs"),
    )
    .unwrap();
    let report = end(&conn, &mut store, started["session"].as_str().unwrap()).unwrap();
    let changes = report["changes"].as_array().unwrap();
    assert_eq!(
        changes
            .iter()
            .filter(|change| change["category"] == "renamed")
            .count(),
        1
    );
    let renamed = changes
        .iter()
        .find(|change| change["category"] == "renamed")
        .unwrap();
    assert_eq!(renamed["path"], "unique.rs");
    assert_eq!(renamed["after_path"], "moved.rs");
    assert!(changes.iter().any(|change| change["path"] == "ambiguous.rs"
        && change["category"] == "addition_without_preimage"));
}

#[test]
fn session_line_insertion_preserves_duplicate_occurrence_identity() {
    let (_directory, mut store, conn) = fixture();
    let source = "fn repeated() {}\nfn repeated() {}\n";
    std::fs::write(store.root.join("a.rs"), source).unwrap();
    let started = start(&conn, &mut store).unwrap();
    let baseline: String = conn
        .query_row(
            "SELECT baseline FROM diagnostic_sessions WHERE id=?1",
            [started["session"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    let baseline: Value = serde_json::from_str(&baseline).unwrap();
    let facts = baseline["files"]["a.rs"]["facts"].as_array().unwrap();
    let key = &facts
        .iter()
        .find(|fact| {
            fact["category"] == "declaration"
                && facts
                    .iter()
                    .filter(|other| other["key"] == fact["key"])
                    .count()
                    == 2
        })
        .unwrap()["key"];
    std::fs::write(store.root.join("a.rs"), format!("// inserted\n{source}")).unwrap();
    let report = end(&conn, &mut store, started["session"].as_str().unwrap()).unwrap();
    let change = &report["changes"][0];
    let source_change = &change["source_changes"]["changes"][0];
    assert_eq!(source_change["before"], json!({"start":0,"end":0}));
    let rows = change["occurrences"]["changes"].as_array().unwrap();
    assert!(rows.iter().all(|row| &row["before"]["key"] != key));
}

#[test]
fn session_baseline_survives_recreated_live_index_at_same_generation() {
    let (directory, mut store, conn) = fixture();
    let root = store.root.clone();
    std::fs::write(root.join("a.rs"), "fn original() {}\n").unwrap();
    let started = start(&conn, &mut store).unwrap();
    drop(store);
    std::fs::remove_file(directory.path().join("cache/index.sqlite3")).unwrap();
    std::fs::write(root.join("a.rs"), "fn replacement() {}\n").unwrap();
    let mut store = Store::open(&root, &directory.path().join("cache")).unwrap();
    let report = end(&conn, &mut store, started["session"].as_str().unwrap()).unwrap();
    assert_eq!(report["before_generation"], report["after_generation"]);
    assert_eq!(report["publication_index_replaced"], true);
    assert_eq!(report["publication_observations"]["complete"], false);
    assert!(
        report["publication_observations"]["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(report["changes"][0]["category"], "modified");
}
