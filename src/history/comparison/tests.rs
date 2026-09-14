use super::*;

#[test]
fn tree_staging_rejects_expanding_paths_and_dense_entries() {
    let oid = "1".repeat(40);
    let long_path = "%".repeat(40_000);
    let mut expanded = Vec::with_capacity(2_000_000);
    for index in 0..40 {
        expanded.extend_from_slice(format!("100644 blob {oid}\t{index}{long_path}\0").as_bytes());
    }
    assert!(expanded.len() < git::MAX_OUTPUT_BYTES);
    assert!(
        parse_tree(&expanded)
            .unwrap_err()
            .to_string()
            .contains("history_resource_limited")
    );
    let mut dense = Vec::with_capacity(1_500_000);
    for index in 0..25_000 {
        dense.extend_from_slice(format!("100644 blob {oid}\tfile_{index}\0").as_bytes());
    }
    assert!(dense.len() < git::MAX_OUTPUT_BYTES);
    assert!(
        parse_tree(&dense)
            .unwrap_err()
            .to_string()
            .contains("history_resource_limited")
    );
}

#[test]
fn cached_change_decode_stops_at_retained_capacity() {
    let row = r#"{"before":null,"after":null,"status":""}"#;
    let payload = format!("[{}]", vec![row; 80_000].join(","));
    assert!(payload.len() < staging::CACHE_BYTES);
    assert!(
        decode_changes(&payload)
            .unwrap_err()
            .to_string()
            .contains("history_resource_limited")
    );
    assert!(
        staging::bounded_json(&vec!["x".repeat(1024); 20], 4096)
            .unwrap_err()
            .to_string()
            .contains("history_resource_limited")
    );
}

#[test]
fn unique_blob_index_matches_many_renames_without_collapsing_duplicates() {
    let mut before = BTreeMap::new();
    let mut after = BTreeMap::new();
    for index in 0..3000 {
        let oid = GitOid::parse(&format!("{index:040x}")).unwrap();
        let old = format!("old_{index}");
        let new = format!("new_{index}");
        before.insert(
            old.clone(),
            TreeFile {
                path: old,
                oid,
                mode: "100644".into(),
            },
        );
        after.insert(
            new.clone(),
            TreeFile {
                path: new,
                oid,
                mode: "100644".into(),
            },
        );
    }
    let duplicate = GitOid::parse(&"a".repeat(40)).unwrap();
    for index in 0..2 {
        let old = format!("duplicate_old_{index}");
        let new = format!("duplicate_new_{index}");
        before.insert(
            old.clone(),
            TreeFile {
                path: old,
                oid: duplicate,
                mode: "100644".into(),
            },
        );
        after.insert(
            new.clone(),
            TreeFile {
                path: new,
                oid: duplicate,
                mode: "100644".into(),
            },
        );
    }
    let changes = compare_trees(before, after).unwrap();
    assert_eq!(
        changes
            .iter()
            .filter(|c| c.status == "renamed_exact_blob")
            .count(),
        3000
    );
    assert_eq!(changes.iter().filter(|c| c.status == "added").count(), 2);
    assert_eq!(changes.iter().filter(|c| c.status == "deleted").count(), 2);
}
