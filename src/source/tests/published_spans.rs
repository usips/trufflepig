use super::*;

#[test]
fn published_afterspan_bounds_show_and_immutable_continuation() {
    let (root, cache, mut store, _) = historical_fixture("sha1");
    let history_cache = tempfile::tempdir().unwrap();
    let history = crate::history::History::open(root.path(), history_cache.path()).unwrap();
    let bytes = format!(
        "fn original() {{\n{}{}{}\n}}\n",
        "    original();\n".repeat(10),
        "    changed();\n".repeat(200),
        "    original();\n".repeat(10)
    );
    std::fs::write(root.path().join("lib.rs"), &bytes).unwrap();
    let response = history
        .since(
            &mut store,
            "HEAD",
            Some("path:lib.rs"),
            true,
            &OutputBudget::new(8000).unwrap(),
        )
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    let after = response["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["entry"] == "live_source")
        .unwrap();
    assert_eq!(after["kind"], "source_region");
    assert!(after["start"].as_u64().unwrap() > 0);
    assert!(after["end"].as_u64().unwrap() < bytes.len() as u64);
    let budget = OutputBudget::new(600).unwrap();
    let first: serde_json::Value =
        serde_json::from_str(&show(&store, after["handle"].as_str().unwrap(), &budget).unwrap())
            .unwrap();
    assert_eq!(first["start"], after["start"]);
    assert_eq!(first["end"], after["end"]);
    assert!(
        first["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["text"] == "    changed();\n")
    );
    let cursor = first["next"].as_str().unwrap().to_owned();
    drop(store);
    let store = Store::open(root.path(), cache.path()).unwrap();
    let second: serde_json::Value =
        serde_json::from_str(&show(&store, &cursor, &budget).unwrap()).unwrap();
    assert_eq!(
        second["start"],
        first["lines"].as_array().unwrap().last().unwrap()["end"]
    );
    assert_eq!(second["end"], after["end"]);
    assert!(
        second["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["text"] == "    changed();\n")
    );
}
