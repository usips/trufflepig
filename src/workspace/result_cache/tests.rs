use super::*;

fn empty_set() -> WorkspaceSet {
    WorkspaceSet {
        workspace: "test".into(),
        home: None,
        owners: vec![],
        coverage: vec![],
        hits: vec![],
        truncated: false,
    }
}
#[test]
fn workspace_result_retention_expiry_and_corrupt_ownership_are_explicit() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let results = WorkspaceResults::open(dir.path())?;
    let first = results.save(empty_set())?;
    results.conn.execute(
        "UPDATE workspace_results SET expires=?1 WHERE id=?2",
        params![results::now() + 1, first],
    )?;
    for _ in 0..50 {
        results.save(empty_set())?;
    }
    assert!(
        results
            .load(&first)
            .unwrap_err()
            .to_string()
            .starts_with("expired_result:")
    );
    let count: i64 = results
        .conn
        .query_row("SELECT count(*) FROM workspace_results", [], |r| r.get(0))?;
    assert_eq!(count, 50);
    let expired = results.save(empty_set())?;
    results.conn.execute(
        "UPDATE workspace_results SET expires=0 WHERE id=?1",
        [&expired],
    )?;
    assert!(
        results
            .load(&expired)
            .unwrap_err()
            .to_string()
            .starts_with("expired_result:")
    );
    let invalid = results.save(empty_set())?;
    let entry = json!({"owner":4,"member_rank":1,"entry":{"entry":"commit","handle":format!("{invalid}:1"),"repository":"root","commit":"a".repeat(40),"parent":null,"summary":"commit","committer_time":0}});
    let mut payload = serde_json::to_value(empty_set())?;
    payload["hits"] = json!([entry]);
    results.conn.execute(
        "UPDATE workspace_results SET payload=?1 WHERE id=?2",
        params![payload.to_string(), invalid],
    )?;
    assert!(
        results
            .load(&invalid)
            .unwrap_err()
            .to_string()
            .starts_with("result_unavailable:")
    );
    Ok(())
}
#[test]
fn workspace_result_metadata_never_exceeds_retention_capacity() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let results = WorkspaceResults::open(dir.path())?;
    let mut oversized = empty_set();
    oversized.coverage = vec![json!({"detail":"x".repeat(results::MAX_BYTES)})];
    assert!(
        results
            .save(oversized)
            .unwrap_err()
            .to_string()
            .starts_with("result_cache_unavailable:")
    );
    let bytes: i64 = results.conn.query_row(
        "SELECT coalesce(sum(length(CAST(payload AS BLOB))),0) FROM workspace_results",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(bytes, 0);
    Ok(())
}

#[test]
fn workspace_pages_use_absolute_file_locators_and_compact_coverage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("member");
    std::fs::create_dir(&root)?;
    let results = WorkspaceResults::open(directory.path().join("cache").as_path())?;
    let hit = crate::results::Hit {
        handle: String::new(),
        path: "src%20file.rs".into(),
        revision: Some("revision".into()),
        start: 2,
        end: 8,
        start_line: 2,
        end_line: 3,
        name: "AName".into(),
        kind: "definition".into(),
        container: Some("Container".into()),
        provenance: Some("exact_identifier".into()),
        resolution: None,
        candidates: Vec::new(),
        target: None,
    };
    let set = WorkspaceSet {
        workspace: "test".into(),
        home: Some("member".into()),
        owners: vec![MemberSnapshot {
            name: "member".into(),
            root: encode_path(&root),
            cache: encode_path(directory.path()),
            device: 0,
            inode: 0,
            index_identity: "epoch".into(),
            generation: 4,
            coverage: json!({}),
        }],
        coverage: vec![json!({
            "member":"member",
            "state":"pending",
            "reason":"index warming",
            "issues":{"semantic_status":"unavailable", "semantic_reason":"failure ".repeat(1000)},
            "detail":{"large":"payload"}
        })],
        hits: vec![OwnedEntry {
            owner: 0,
            member_rank: 1,
            entry: ResultEntry::LiveSource(hit),
        }],
        truncated: false,
    };
    let id = results.save(set)?;
    let value: Value = serde_json::from_str(&results.page(&id, 0, 1, &OutputBudget::new(600)?)?)?;
    assert_eq!(
        value["hits"][0]["file"],
        format!("file://{}", encode_path(&root.join("src file.rs")))
    );
    assert_eq!(value["hits"][0]["member"], "member");
    assert!(value["hits"][0].get("revision").is_none());
    assert!(value.get("members").is_none());
    assert_eq!(value["coverage"][0]["state"], "pending");
    assert_eq!(value["coverage"][0]["reason"], "index warming");
    assert!(value["coverage"][0].get("detail").is_none());
    assert_eq!(
        value["coverage"][0]["issues"]["semantic_status"],
        "unavailable"
    );
    assert!(
        value["coverage"][0]["issues"]["semantic_reason"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 121
    );
    Ok(())
}
