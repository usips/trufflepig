use super::*;

#[test]
fn linked_oid_lookup_has_no_plan_window_and_uses_exact_repo_and_oid() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE commit_tasks(repo_key TEXT, oid TEXT, plan_id INTEGER, task_ordinal INTEGER)",
    )
    .unwrap();
    let repo = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let other_repo = RepoKey::from_roots([GitOid::parse(&"f".repeat(40)).unwrap()]).unwrap();
    let first = GitOid::parse(&"b".repeat(40)).unwrap();
    let second = GitOid::parse(&"c".repeat(40)).unwrap();
    let foreign = GitOid::parse(&"d".repeat(40)).unwrap();
    for (repo_key, oid, plan, task) in [
        (&repo, first, 1, 1),
        (&repo, first, 77, 2),
        (&repo, second, 77, 3),
        (&other_repo, foreign, 1, 1),
    ] {
        conn.execute(
            "INSERT INTO commit_tasks VALUES(?1,?2,?3,?4)",
            params![repo_key.as_str(), oid.as_str(), plan, task],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO commit_tasks VALUES(?1,?2,1,4)",
        params![repo.as_str(), format!("{foreign}0")],
    )
    .unwrap();
    conn.pragma_update(None, "query_only", true).unwrap();
    let mut candidates = Vec::with_capacity(2203);
    candidates.extend([first, second, foreign]);
    candidates.extend((0..2200).map(|index| GitOid::parse(&format!("{index:040x}")).unwrap()));
    assert_eq!(
        linked_commit_oids(&conn, &repo, &candidates).unwrap(),
        BTreeSet::from([first, second]),
        "all task plans count; another repo and a longer oid do not"
    );
    assert!(linked_commit_oids(&conn, &repo, &[]).unwrap().is_empty());
}
