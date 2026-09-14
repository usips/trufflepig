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
