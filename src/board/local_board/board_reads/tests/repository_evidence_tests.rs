use super::*;

#[test]
fn repositories_keep_all_plan_links_each_path_and_the_oldest_plan_boundary() {
    let (directory, mut board) = database();
    let first = new_plan(&mut board);
    let BoardResult::Change(second) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Other plan").unwrap(),
            body: PlanText::new("Other scope").unwrap(),
            steward: None,
            repo_key: None,
        },
    ) else {
        panic!("missing second plan")
    };
    let second = second.plan.unwrap();
    board
        .conn
        .execute(
            "UPDATE plans SET created_at=CASE id WHEN 1 THEN 120 ELSE 90 END",
            [],
        )
        .unwrap();
    let repo_key: RepoKey = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
    let first_path = directory.path().join("one.git");
    let second_path = directory.path().join("two.git");
    std::fs::create_dir_all(&first_path).unwrap();
    std::fs::create_dir_all(&second_path).unwrap();
    for (plan, path) in [(first, &first_path), (second, &second_path)] {
        call(
            &mut board,
            "codex",
            BoardOp::RegisterRepo {
                registration: RepoRegistration {
                    root_commits: Vec::new(),
                    registration_error: None,
                    origin_override: None,
                    repo_key: repo_key.clone(),
                    origin_label: Some("origin".to_owned()),
                    host: "laptop".to_owned(),
                    common_dir: path.to_owned(),
                    plan_id: Some(plan),
                },
            },
        );
    }
    board
        .conn
        .execute(
            "UPDATE repo_paths SET scan_error='failed scan' WHERE common_dir=?1",
            [second_path.to_str().unwrap()],
        )
        .unwrap();
    let BoardResult::Repositories(targets) = call(
        &mut board,
        "codex",
        BoardOp::Repositories { plan: Some(first) },
    ) else {
        panic!("missing repositories")
    };
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().all(|target| target.plans == [first, second]));
    assert!(targets.iter().all(|target| target.oldest_plan_at == 90));
    assert!(
        targets
            .iter()
            .all(|target| target.registration.plan_id == Some(first))
    );
    assert!(
        targets
            .iter()
            .any(|target| target.scan_error.as_deref() == Some("failed scan"))
    );
}

#[test]
fn repository_reads_sanitize_legacy_origins_without_rewriting_evidence() {
    let (directory, mut board) = database();
    let repo = "a".repeat(40);
    let root_oid = "b".repeat(40);
    let original = "https://legacy-user:legacy-password@example.invalid/team/repository.git";
    board
        .conn
        .execute(
            "INSERT INTO repos(repo_key,origin_label) VALUES(?1,?2)",
            params![repo, original],
        )
        .unwrap();
    board.conn.execute("INSERT INTO repo_paths(repo_key,host,common_dir,root_commits_json,registration_error) VALUES(?1,'laptop',?2,?3,'registration warning')", params![repo, directory.path().join("legacy.git").to_str().unwrap(), serde_json::to_string(&vec![root_oid.clone()]).unwrap()]).unwrap();
    let changes = board.conn.total_changes();
    let BoardResult::Repositories(targets) =
        call(&mut board, "codex", BoardOp::Repositories { plan: None })
    else {
        panic!("missing repositories")
    };
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0].registration.origin_label.as_deref(),
        Some("https://example.invalid/team/repository.git")
    );
    assert_eq!(
        targets[0].registration.root_commits[0].to_string(),
        root_oid
    );
    assert_eq!(
        targets[0].registration.registration_error.as_deref(),
        Some("registration warning")
    );
    assert_eq!(targets[0].registration.origin_override, None);
    let encoded = serde_json::to_string(&targets).unwrap();
    assert!(!encoded.contains("legacy-user") && !encoded.contains("legacy-password"));
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT origin_label FROM repos WHERE repo_key=?1",
                [repo],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        original
    );
    assert_eq!(board.conn.total_changes(), changes);
}
