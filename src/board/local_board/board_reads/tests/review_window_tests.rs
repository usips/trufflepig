use super::*;

#[test]
fn review_preserves_overlapping_ended_claims_and_other_agents_open_work() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    for ordinal in 1..=2 {
        call(
            &mut board,
            "human",
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new(format!("Lane {ordinal}")).unwrap(),
                to: None,
                section: None,
            },
        );
        let task = TaskId::new(plan, ordinal).unwrap();
        call(
            &mut board,
            "claude",
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new(format!("scope {ordinal}")).unwrap()),
                resume: false,
            },
        );
        call(
            &mut board,
            "claude",
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Review,
                to: None,
            },
        );
    }
    board.conn.execute("UPDATE entries SET created_at=120 WHERE id=(SELECT entry_id FROM revisions WHERE plan_id=1 AND number=1)", []).unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET claimed_at=110,ended_at=130 WHERE task_ordinal=1",
            [],
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET claimed_at=100,ended_at=119 WHERE task_ordinal=2",
            [],
        )
        .unwrap();
    let question = post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "Review this unresolved point",
    );
    call(
        &mut board,
        "claude",
        BoardOp::Propose {
            supersedes: None,
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("proposal").unwrap(),
            summary: EntryText::new("proposal summary").unwrap(),
        },
    );
    call(
        &mut board,
        "codex",
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("missing evidence").unwrap(),
            body: None,
            plan: Some(plan),
            metadata: FeedbackMetadata::default(),
            import_key: None,
        },
    );
    let BoardResult::Review(evidence) = call(
        &mut board,
        "codex",
        BoardOp::Review {
            base: PlanRevision::new(plan, 1).unwrap(),
            agent: Some(HarnessLabel::parse("codex").unwrap()),
        },
    ) else {
        panic!("missing review")
    };
    assert_eq!(evidence.claims.len(), 1);
    assert_eq!(evidence.claims[0].scope.as_str(), "scope 1");
    assert_eq!(evidence.claims[0].ended_at, Some(130));
    assert_eq!(evidence.open_questions[0].id, question);
    assert_eq!(evidence.open_proposals.len(), 1);
    assert_eq!(evidence.open_feedback.len(), 1);
    assert!(
        evidence
            .entries
            .iter()
            .all(|entry| entry.actor.harness.as_str() == "codex")
    );
}

#[test]
fn review_commits_keep_coauthors_and_links_to_other_plans() {
    let (directory, mut board) = database();
    let first = new_plan(&mut board);
    let BoardResult::Change(second) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Other plan").unwrap(),
            body: PlanText::new("Other scope").unwrap(),
            steward: None,
        },
    ) else {
        panic!("missing second plan")
    };
    let second = second.plan.unwrap();
    board.conn.execute("UPDATE entries SET created_at=120 WHERE id=(SELECT entry_id FROM revisions WHERE plan_id=1 AND number=1)", []).unwrap();
    let repo_key: RepoKey = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
    call(
        &mut board,
        "codex",
        BoardOp::RegisterRepo {
            registration: RepoRegistration {
                root_commits: Vec::new(),
                registration_error: None,
                origin_override: None,
                repo_key: repo_key.clone(),
                origin_label: None,
                host: "laptop".to_owned(),
                common_dir: directory.path().to_owned(),
                plan_id: Some(first),
            },
        },
    );
    let commit = LinkedCommit {
        repo_key,
        oid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".parse().unwrap(),
        subject: "Implement both plans".to_owned(),
        committed_at: 125,
        author: "Josh".to_owned(),
        coauthors: vec![CommitCoauthor {
            harness: HarnessLabel::parse("codex").unwrap(),
            model: "gpt-6.1-sol".to_owned(),
            email: "noreply@openai.com".to_owned(),
        }],
        files: 1,
        insertions: 3,
        deletions: 2,
        plans: vec![
            CommitPlanLink {
                plan_id: first,
                task_ordinal: None,
            },
            CommitPlanLink {
                plan_id: second,
                task_ordinal: None,
            },
        ],
    };
    call(
        &mut board,
        "codex",
        BoardOp::LinkCommits {
            commits: vec![commit.clone()],
        },
    );
    let BoardResult::Review(evidence) = call(
        &mut board,
        "codex",
        BoardOp::Review {
            base: PlanRevision::new(first, 1).unwrap(),
            agent: None,
        },
    ) else {
        panic!("missing review")
    };
    assert_eq!(evidence.commits, [commit]);
}
