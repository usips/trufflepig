use super::*;
use crate::{search, source};

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("lib.rs"),
        "fn alpha() {}\nfn beta() { alpha(); }\n",
    )
    .unwrap();
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    (root, cache, store)
}

fn query(store: &Store, text: &str) -> ResultSet {
    search::search(
        store,
        &search::Query::parse(text).unwrap(),
        false,
        std::path::Path::new("."),
    )
    .unwrap()
}

#[test]
fn handles_survive_interleaving_and_reopening() {
    let (root, cache, mut store) = fixture();
    let a = query(&store, "sym:alpha");
    let id = save(&mut store, a).unwrap();
    let b = query(&store, "sym:beta");
    save(&mut store, b).unwrap();
    drop(store);
    let store = Store::open(root.path(), cache.path()).unwrap();
    let (_, hit) = handle(&store, &format!("{id}:1")).unwrap();
    assert_eq!(hit.name, "alpha");
    assert!(
        source::show(&store, &hit.handle, &OutputBudget::new(600).unwrap())
            .unwrap()
            .contains("fn alpha")
    );
}

#[test]
fn expired_evicted_and_invalid_handles_never_redirect() {
    let (_root, _cache, mut store) = fixture();
    let set = query(&store, "sym:alpha");
    let id = save(&mut store, set).unwrap();
    store
        .conn
        .execute("UPDATE result_sets SET expires=0 WHERE id=?1", [&id])
        .unwrap();
    assert!(
        load(&store, &id)
            .unwrap_err()
            .to_string()
            .contains("expired_result")
    );
    assert!(handle(&store, &format!("{id}:0")).is_err());
    for _ in 0..51 {
        let set = query(&store, "sym:beta");
        save(&mut store, set).unwrap();
    }
    assert!(handle(&store, &format!("{id}:1")).is_err());
    let count: i64 = store
        .conn
        .query_row("SELECT count(*) FROM result_sets", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 50);
}

#[test]
fn stale_source_and_graph_are_explicit() {
    let (root, _cache, mut store) = fixture();
    let set = query(&store, "sym:alpha");
    let id = save(&mut store, set).unwrap();
    let budget = OutputBudget::new(600).unwrap();
    std::fs::write(root.path().join("lib.rs"), "// moved\nfn alpha() {}\n").unwrap();
    assert!(
        source::show(&store, &format!("{id}:1"), &budget)
            .unwrap_err()
            .to_string()
            .contains("stale_source")
    );
    assert!(
        source::show(&store, "path:lib.rs:2-2", &budget)
            .unwrap()
            .contains("fn alpha")
    );
    store.index().unwrap();
    assert!(
        search::context(&store, &format!("{id}:1"), &budget)
            .unwrap_err()
            .to_string()
            .contains("stale_result")
    );
    std::fs::remove_file(root.path().join("lib.rs")).unwrap();
    assert!(source::show(&store, &format!("{id}:1"), &budget).is_err());
}

#[test]
fn pagination_respects_full_budget_and_advances_explicitly() {
    let (_root, _cache, mut store) = fixture();
    let set = query(&store, "");
    let id = save(&mut store, set).unwrap();
    let budget = OutputBudget::new(600).unwrap();
    let first = page(&store, &id, 0, 1, &budget).unwrap();
    assert!(budget.fits(&first));
    let json: Value = serde_json::from_str(&first).unwrap();
    if let Some(cursor) = json["next"].as_str() {
        let second = more(&store, cursor, 1, &budget).unwrap();
        let second: Value = serde_json::from_str(&second).unwrap();
        assert_ne!(json["hits"][0]["handle"], second["hits"][0]["handle"]);
    }
    assert!(page(&store, &id, 0, 20, &OutputBudget::new(1).unwrap()).is_err());
}

#[test]
fn cache_bytes_and_hit_count_are_bounded() {
    let (_root, _cache, mut store) = fixture();
    let mut set = query(&store, "sym:alpha");
    let hit = set.hits[0].clone();
    set.hits = vec![hit; MAX_HITS + 1];
    let id = save(&mut store, set).unwrap();
    let set = load(&store, &id).unwrap();
    assert_eq!(set.hits.len(), MAX_HITS);
    assert!(set.truncated);
    let budget = OutputBudget::new(600).unwrap();
    assert!(budget.fits(&page(&store, &id, 0, MAX_HITS, &budget).unwrap()));
}

#[test]
fn candidate_pages_report_omissions_and_allow_larger_replay() {
    let (_root, _cache, mut store) = fixture();
    let mut set = query(&store, "sym:alpha");
    let candidate = DefinitionTarget {
        path: "lib.rs".into(),
        revision: "f".repeat(64),
        start: 0,
        end: 12,
        name: "alpha".into(),
    };
    set.hits[0].candidates = vec![candidate; 64];
    set.hits[0].resolution = Some("candidate".into());
    let id = save(&mut store, set).unwrap();
    let response = page(&store, &id, 0, 1, &OutputBudget::new(600).unwrap()).unwrap();
    let value: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["hits"][0]["candidates_truncated"], true);
    assert_eq!(value["hits"][0]["candidates_total"], 64);
    assert!(
        !value["hits"][0]["candidates"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let response = page(&store, &id, 0, 1, &OutputBudget::new(10000).unwrap()).unwrap();
    let value: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["hits"][0]["candidates"].as_array().unwrap().len(), 64);
}

#[test]
fn historical_entries_share_cache_and_reject_live_context() {
    let (_root, _cache, store) = fixture();
    let oid = crate::identity::GitOid::parse(&"a".repeat(40)).unwrap();
    let change = ChangeEntry {
        handle: String::new(),
        name: "alpha".into(),
        status: "modified".into(),
        before: None,
        after: Some(HistoricalSource {
            repository: "/repo/.git".into(),
            commit: oid,
            blob: oid,
            revision: crate::identity::ContentRevision::of(b""),
            path: "lib.rs".into(),
            span: crate::identity::ByteSpan::new(0, 12).unwrap(),
        }),
        correspondence: "exact".into(),
    };
    let id = save_entries(
        &store,
        1,
        serde_json::json!({}),
        vec![ResultEntry::Change(change)],
        false,
    )
    .unwrap();
    let handle = format!("{id}:1");
    assert!(matches!(
        entry(&store, &handle).unwrap().1,
        ResultEntry::Change(_)
    ));
    let budget = OutputBudget::new(600).unwrap();
    let response: Value = serde_json::from_str(&page(&store, &id, 0, 1, &budget).unwrap()).unwrap();
    assert_eq!(response["hits"][0]["entry"], "change");
    assert!(
        search::context(&store, &handle, &budget)
            .unwrap_err()
            .to_string()
            .contains("historical_result")
    );
    assert!(
        source::show(&store, &handle, &budget)
            .unwrap_err()
            .to_string()
            .contains("side_required")
    );
}

#[test]
fn immutable_continuation_preserves_range_across_restart_and_edits() {
    let (root, cache, mut store) = fixture();
    let source = (0..250)
        .map(|i| format!("// line {i}\n"))
        .collect::<String>();
    std::fs::write(root.path().join("large.rs"), &source).unwrap();
    store.index().unwrap();
    let budget = OutputBudget::new(600).unwrap();
    let page: Value =
        serde_json::from_str(&source::show(&store, "path:large.rs:3-220", &budget).unwrap())
            .unwrap();
    let cursor = page["next"].as_str().unwrap().to_owned();
    assert!(cursor.starts_with("read:"));
    assert_eq!(page["verified"], false);
    drop(store);
    let store = Store::open(root.path(), cache.path()).unwrap();
    let next: Value =
        serde_json::from_str(&source::show(&store, &cursor, &budget).unwrap()).unwrap();
    assert_eq!(next["verified"], true);
    assert_eq!(next["revision"], page["revision"]);
    assert_eq!(
        next["start"],
        page["lines"].as_array().unwrap().last().unwrap()["end"]
    );
    assert_eq!(next["end"], page["end"]);
    let outside = format!("{}@0", cursor.rsplit_once('@').unwrap().0);
    assert!(
        source::show(&store, &outside, &budget)
            .unwrap_err()
            .to_string()
            .contains("invalid_cursor")
    );
    std::fs::write(
        root.path().join("large.rs"),
        format!("// changed\n{source}"),
    )
    .unwrap();
    assert!(
        source::show(&store, &cursor, &budget)
            .unwrap_err()
            .to_string()
            .contains("stale_source")
    );
}

#[test]
fn live_handle_continuation_expires_with_original_set() {
    let (root, _cache, mut store) = fixture();
    let source = format!("fn alpha() {{\n{}\n}}\n", "    call();\n".repeat(210));
    std::fs::write(root.path().join("lib.rs"), source).unwrap();
    store.index().unwrap();
    let set = query(&store, "sym:alpha");
    let id = save(&mut store, set).unwrap();
    let budget = OutputBudget::new(600).unwrap();
    let page: Value =
        serde_json::from_str(&source::show(&store, &format!("{id}:1"), &budget).unwrap()).unwrap();
    let cursor = page["next"].as_str().unwrap();
    assert!(cursor.starts_with(&format!("read:{id}:1@")));
    store
        .conn
        .execute("UPDATE result_sets SET expires=0 WHERE id=?1", [&id])
        .unwrap();
    assert!(
        source::show(&store, cursor, &budget)
            .unwrap_err()
            .to_string()
            .contains("expired_result")
    );
}
