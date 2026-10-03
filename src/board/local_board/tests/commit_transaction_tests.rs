use super::*;

#[test]
fn local_board_commit_batch_is_atomic_idempotent_and_emits_one_event() {
    let (mut board, path) = database();
    let author = actor("human", "h1");
    let plan = new_plan(&mut board, author.clone(), "Trial").plan.unwrap();
    let root = crate::identity::GitOid::parse("1111111111111111111111111111111111111111").unwrap();
    let repo_key = RepoKey::from_roots([root]).unwrap();
    let commit = |oid: &str| LinkedCommit {
        repo_key: repo_key.clone(),
        oid: crate::identity::GitOid::parse(oid).unwrap(),
        subject: "implement task".to_owned(),
        committed_at: unix_now().unwrap(),
        author: "Josh".to_owned(),
        coauthors: vec![CommitCoauthor {
            harness: HarnessLabel::parse("codex").unwrap(),
            model: "Model".to_owned(),
            email: "noreply@openai.com".to_owned(),
        }],
        files: 1,
        insertions: 2,
        deletions: 1,
        plans: vec![CommitPlanLink {
            plan_id: plan,
            task_ordinal: None,
        }],
    };
    let request = BoardRequest::new(
        author,
        BoardOp::LinkCommits {
            commits: vec![
                commit("2222222222222222222222222222222222222222"),
                commit("3333333333333333333333333333333333333333"),
            ],
        },
    );
    let before = board.max_seq().unwrap().get();
    let linked = board.handle(&request).unwrap();
    let BoardResult::CommitsLinked(linked) = linked.result else {
        panic!("links");
    };
    assert_eq!(linked.inserted, 2);
    assert_eq!(board.max_seq().unwrap().get(), before + 1);
    let linked = board.handle(&request).unwrap();
    let BoardResult::CommitsLinked(linked) = linked.result else {
        panic!("links");
    };
    assert_eq!(linked.inserted, 0);
    assert_eq!(board.max_seq().unwrap().get(), before + 1);
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
