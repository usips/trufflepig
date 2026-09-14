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
