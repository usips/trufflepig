use super::*;

fn plan_only_commit(repo_key: RepoKey, oid: &str, plan: PlanId, committed_at: i64) -> LinkedCommit {
    LinkedCommit {
        repo_key,
        oid: oid.parse().unwrap(),
        subject: "Plan-only scanned commit".to_owned(),
        committed_at,
        author: "Fixture <fixture@example.test>".to_owned(),
        coauthors: Vec::new(),
        files: 1,
        insertions: 2,
        deletions: 0,
        plans: vec![CommitPlanLink {
            plan_id: plan,
            task_ordinal: None,
        }],
    }
}

fn scan_plan_only(board: &mut LocalBoard, commit: LinkedCommit) {
    let result = call(
        board,
        "codex",
        BoardOp::LinkCommits {
            commits: vec![commit],
        },
    );
    assert!(matches!(result, BoardResult::CommitsLinked(_)));
}

fn repair_task_link(
    board: &mut LocalBoard,
    actor: BoardActor,
    task: TaskId,
    commit: &LinkedCommit,
) -> (EntryId, EventSeq) {
    let BoardResult::Change(change) = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::LinkCommit {
                oid: commit.oid,
                task,
                resolution: Some(Box::new(commit.clone())),
            },
        ))
        .unwrap()
        .result
    else {
        panic!("manual link did not return a change")
    };
    (change.entry, change.seq)
}

fn reviewer(harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

#[test]
fn review_keeps_manual_linkers_outside_entry_filters_and_marks_unknown_history() {
    let (_directory, mut board) = database();
    let BoardResult::Change(created) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Manual review evidence").unwrap(),
            body: PlanText::new("Manual review evidence").unwrap(),
            steward: Some(HarnessLabel::parse("codex").unwrap()),
            repo_key: None,
        },
    ) else {
        panic!("plan creation failed")
    };
    let plan = created.plan.unwrap();
    call(
        &mut board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Reviewed task").unwrap(),
            to: None,
            section: None,
        },
    );
    let task = TaskId::new(plan, 1).unwrap();
    let repo_root = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
    let repo_key = RepoKey::from_roots([repo_root]).unwrap();
    let before_base = plan_only_commit(
        repo_key.clone(),
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        plan,
        125,
    );
    scan_plan_only(&mut board, before_base.clone());
    let (before_entry, before_seq) = repair_task_link(
        &mut board,
        reviewer("human", "early-linker"),
        task,
        &before_base,
    );

    call(
        &mut board,
        "human",
        BoardOp::Edit {
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("Revision after the first repair").unwrap(),
            summary: EntryText::new("Set review base after manual repair").unwrap(),
        },
    );
    board
        .conn
        .execute(
            "UPDATE entries SET created_at=120 WHERE id=(SELECT entry_id FROM revisions WHERE plan_id=?1 AND number=2)",
            [plan.get() as i64],
        )
        .unwrap();
    let base = PlanRevision::new(plan, 2).unwrap();

    let after_base = plan_only_commit(
        repo_key.clone(),
        "cccccccccccccccccccccccccccccccccccccccc",
        plan,
        126,
    );
    scan_plan_only(&mut board, after_base.clone());
    let (_, after_seq) = repair_task_link(
        &mut board,
        reviewer("codex", "steward-linker"),
        task,
        &after_base,
    );

    let unknown_history = plan_only_commit(
        repo_key.clone(),
        "dddddddddddddddddddddddddddddddddddddddd",
        plan,
        127,
    );
    scan_plan_only(&mut board, unknown_history.clone());
    repair_task_link(
        &mut board,
        reviewer("human", "unknown-history-linker"),
        task,
        &unknown_history,
    );
    board
        .conn
        .execute(
            "UPDATE commit_tasks SET link_seq=NULL WHERE repo_key=?1 AND oid=?2 AND plan_id=?3 AND task_ordinal=?4",
            rusqlite::params![
                unknown_history.repo_key.as_str(),
                unknown_history.oid.as_str(),
                plan.get() as i64,
                task.ordinal as i64
            ],
        )
        .unwrap();

    let before_window = plan_only_commit(
        repo_key.clone(),
        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        plan,
        119,
    );
    scan_plan_only(&mut board, before_window.clone());
    repair_task_link(
        &mut board,
        reviewer("human", "before-window-linker"),
        task,
        &before_window,
    );

    let after_window_end = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 86_400;
    let after_window = plan_only_commit(
        repo_key,
        "ffffffffffffffffffffffffffffffffffffffff",
        plan,
        after_window_end,
    );
    scan_plan_only(&mut board, after_window.clone());
    repair_task_link(
        &mut board,
        reviewer("codex", "after-window-linker"),
        task,
        &after_window,
    );

    let BoardResult::Review(all_review) =
        call(&mut board, "codex", BoardOp::Review { base, agent: None })
    else {
        panic!("unfiltered review missing")
    };
    let BoardResult::Review(codex_review) = call(
        &mut board,
        "codex",
        BoardOp::Review {
            base,
            agent: Some(HarnessLabel::parse("codex").unwrap()),
        },
    ) else {
        panic!("codex review missing")
    };

    assert_eq!(all_review.commits.len(), 3);
    assert_eq!(all_review.manual_links.len(), 3);
    for excluded in [&before_window, &after_window] {
        assert!(
            !all_review
                .commits
                .iter()
                .any(|commit| commit.oid == excluded.oid),
            "commit {} is outside the review window",
            excluded.oid
        );
        assert!(
            !all_review
                .manual_links
                .iter()
                .any(|link| link.oid == excluded.oid),
            "manual link {} is outside the review window",
            excluded.oid
        );
    }
    assert_eq!(all_review.manual_links, codex_review.manual_links);
    assert!(
        !all_review
            .entries
            .iter()
            .any(|entry| entry.id == before_entry)
    );
    assert!(
        !codex_review
            .entries
            .iter()
            .any(|entry| entry.id == before_entry)
    );

    let before = all_review
        .manual_links
        .iter()
        .find(|link| link.oid == before_base.oid)
        .unwrap();
    assert_eq!(before.task, task);
    assert_eq!(before.entry, before_entry);
    assert_eq!(before.seq, Some(before_seq));
    assert_eq!(
        before.actor.as_ref(),
        Some(&reviewer("human", "early-linker"))
    );
    assert!(before.seq.unwrap() < all_review.base.seq);

    let after = all_review
        .manual_links
        .iter()
        .find(|link| link.oid == after_base.oid)
        .unwrap();
    assert_eq!(after.seq, Some(after_seq));
    assert_eq!(
        after.actor.as_ref(),
        Some(&reviewer("codex", "steward-linker"))
    );
    assert!(after.seq.unwrap() > all_review.base.seq);

    let unknown = all_review
        .manual_links
        .iter()
        .find(|link| link.oid == unknown_history.oid)
        .unwrap();
    assert_eq!(unknown.seq, None);
    assert_eq!(unknown.actor, None);
}
