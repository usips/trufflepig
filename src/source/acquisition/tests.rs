use super::*;
use crate::output::OutputBudget;

#[test]
fn owned_read_survives_member_result_eviction_and_keeps_provenance() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let bytes = "// immutable source line\n".repeat(220);
    std::fs::write(root.path().join("lib.rs"), &bytes).unwrap();
    let store = Store::open(root.path(), cache.path()).unwrap();
    let original = acquire(&store, "path:lib.rs", None, &InvocationDirectory::root()).unwrap();
    let (_, entry) = results::entry(&store, &original.handle).unwrap();
    store
        .result_sets()
        .unwrap()
        .conn
        .execute("DELETE FROM result_sets", [])
        .unwrap();

    let handle = format!("{}:1", uuid::Uuid::new_v4());
    let source = acquire_entry(&store, &handle, entry.clone(), None).unwrap();
    let metadata =
        serde_json::json!({"member":"engine","members":[{"name":"engine","root":root.path()}]});
    let budget = OutputBudget::new(600).unwrap();
    let output = crate::source::render_owned(source, &budget, &metadata).unwrap();
    assert!(budget.fits(&output));
    let response: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(response["member"], metadata["member"]);
    assert_eq!(response["members"], metadata["members"]);
    assert!(
        response["next"]
            .as_str()
            .unwrap()
            .starts_with(&format!("read:{handle}@"))
    );

    let source = acquire_entry(&store, &handle, entry.clone(), None).unwrap();
    let oversized = serde_json::json!({"member":"engine ".repeat(1000)});
    assert!(
        crate::source::render_owned(source, &budget, &oversized)
            .unwrap_err()
            .to_string()
            .contains("budget_too_small")
    );

    std::fs::write(root.path().join("lib.rs"), "// replacement\n").unwrap();
    assert!(
        acquire_entry(&store, &handle, entry, None)
            .err()
            .unwrap()
            .to_string()
            .contains("stale_source")
    );
}

#[test]
fn owned_source_metadata_cannot_replace_verified_identity() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn original() {}\n").unwrap();
    let store = Store::open(root.path(), cache.path()).unwrap();
    let source = acquire(&store, "path:lib.rs", None, &InvocationDirectory::root()).unwrap();
    let error = crate::source::render_owned(
        source,
        &OutputBudget::new(600).unwrap(),
        &serde_json::json!({"path":"other.rs"}),
    )
    .unwrap_err();
    assert!(error.to_string().contains("invalid_metadata"));
}
