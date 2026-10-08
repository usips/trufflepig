use super::*;
use crate::board::board_actor::BoardRecipient;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) struct ScopeFixture {
    pub(super) board: LocalBoard,
    _directory: tempfile::TempDir,
    pub(super) plans: [PlanId; 4],
    pub(super) keys: [RepoKey; 3],
}

impl ScopeFixture {
    pub(super) fn new() -> Self {
        let directory = crate::board::board_test_support::scratch("project-scope-");
        let mut board = LocalBoard::open_path(
            &directory.path().join("board.sqlite3"),
            Duration::from_secs(120),
        )
        .unwrap();
        let plans = ["A", "B", "C", "Unscoped"].map(|title| {
            new_plan(&mut board, actor("human", "owner"), title)
                .plan
                .unwrap()
        });
        let keys = ['1', '2', '3'].map(|digit| {
            RepoKey::from_roots([
                crate::identity::GitOid::parse(&digit.to_string().repeat(40)).unwrap(),
            ])
            .unwrap()
        });
        for (plan, key) in plans.iter().zip(&keys) {
            board
                .conn
                .execute("INSERT INTO repos(repo_key) VALUES(?1)", [key.as_str()])
                .unwrap();
            board
                .conn
                .execute(
                    "INSERT INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
                    params![sql_number(plan.get()), key.as_str()],
                )
                .unwrap();
        }
        for plan in plans {
            board
                .handle(&BoardRequest::new(
                    actor("claude", "sender"),
                    BoardOp::Post {
                        target: BoardRef::Plan(plan),
                        kind: EntryKind::Question,
                        body: EntryText::new(format!("addressed question {plan}")).unwrap(),
                        to: Some(BoardRecipient::parse("codex").unwrap()),
                        supersedes: None,
                    },
                ))
                .unwrap();
            let created = board
                .handle(&BoardRequest::new(
                    actor("human", "owner"),
                    BoardOp::TaskCreate {
                        plan,
                        title: PlanTitle::new(format!("work {plan}")).unwrap(),
                        to: None,
                        section: None,
                    },
                ))
                .unwrap();
            let BoardResult::Change(created) = created.result else {
                panic!("task creation result");
            };
            board
                .handle(&BoardRequest::new(
                    actor("codex", "reader"),
                    BoardOp::ClaimTask {
                        task: created.task.unwrap(),
                        scope: Some(EntryText::new("scope work").unwrap()),
                        resume: ClaimResume::No,
                        delegate: None,
                    },
                ))
                .unwrap();
            Self::feedback(&mut board, Some(plan), &format!("owned feedback {plan}"));
        }
        Self::feedback(&mut board, None, "planless owned feedback");
        board
            .conn
            .execute("UPDATE claims SET last_active=0", [])
            .unwrap();
        Self {
            board,
            _directory: directory,
            plans,
            keys,
        }
    }

    fn feedback(board: &mut LocalBoard, plan: Option<PlanId>, summary: &str) {
        let reply = board
            .handle(&BoardRequest::new(
                actor("codex", "reader"),
                BoardOp::Feedback {
                    kind: crate::board::board_vocabulary::FeedbackKind::Wrong,
                    summary: EntryText::new(summary).unwrap(),
                    body: None,
                    plan,
                    metadata: FeedbackMetadata::default(),
                    import_key: None,
                },
            ))
            .unwrap();
        let BoardResult::Change(change) = reply.result else {
            panic!("feedback creation result");
        };
        board
            .handle(&BoardRequest::new(
                actor("human", "owner"),
                BoardOp::FeedbackTriage {
                    entry: change.entry,
                    note: None,
                },
            ))
            .unwrap();
    }

    pub(super) fn read(&mut self, scope: Value, mut op: Value) -> BoardReply {
        op["scope"] = scope;
        let request: BoardRequest = serde_json::from_value(json!({
            "api": crate::board::BOARD_API,
            "actor": actor("codex", "reader"),
            "op": op,
        }))
        .expect("scoped read wire request must deserialize through BoardRequest");
        self.board.handle(&request).unwrap()
    }

    pub(super) fn assert_reads(&mut self, scope: Value, admitted: &[PlanId]) {
        let expected: BTreeSet<_> = admitted.iter().copied().collect();
        let reply = self.read(
            scope.clone(),
            json!({
                "op":"overview", "after":null, "through":null, "limit":100,
            }),
        );
        let BoardResult::Overview(overview) = reply.result else {
            panic!("overview result");
        };
        assert_eq!(
            overview
                .plans
                .iter()
                .map(|row| row.plan.id)
                .collect::<BTreeSet<_>>(),
            expected
        );
        let reply = self.read(
            scope.clone(),
            json!({
                "op":"attention", "after":null, "through":null, "limit":100,
            }),
        );
        let BoardResult::Attention(attention) = reply.result else {
            panic!("attention result");
        };
        assert_eq!(
            attention
                .entries
                .iter()
                .filter_map(|row| row.plan)
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert!(
            attention
                .entries
                .iter()
                .all(|row| row.plan.is_some_and(|plan| expected.contains(&plan)))
        );
        assert_eq!(
            attention
                .stale_claims
                .iter()
                .map(|row| row.claim.task.plan)
                .collect::<BTreeSet<_>>(),
            expected
        );
        let reply = self.read(
            scope.clone(),
            json!({
                "op":"inbox", "after":0, "limit":100,
            }),
        );
        let BoardResult::Inbox(inbox) = reply.result else {
            panic!("inbox result");
        };
        assert_eq!(
            inbox
                .events
                .iter()
                .filter_map(|row| row.plan)
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert!(
            inbox
                .events
                .iter()
                .all(|row| row.plan.is_some_and(|plan| expected.contains(&plan)))
        );
        assert_eq!(
            inbox
                .open
                .iter()
                .filter_map(|row| row.plan)
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert!(
            inbox
                .open
                .iter()
                .all(|row| row.plan.is_some_and(|plan| expected.contains(&plan)))
        );
        let reply = self.read(scope.clone(), json!({
            "op":"claims", "plan":null, "own_stale":true, "after":null, "through":null, "limit":100,
        }));
        let BoardResult::Claims(claims) = reply.result else {
            panic!("claims result");
        };
        assert_eq!(
            claims
                .claims
                .iter()
                .map(|row| row.claim.task.plan)
                .collect::<BTreeSet<_>>(),
            expected
        );
        let reply = self.read(
            scope,
            json!({
                "op":"feed", "plan":null, "after":null, "through":null, "limit":100,
            }),
        );
        let BoardResult::Feed(feed) = reply.result else {
            panic!("feed result");
        };
        assert_eq!(
            feed.events
                .iter()
                .filter_map(|row| row.plan)
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert!(
            feed.events
                .iter()
                .all(|row| row.plan.is_some_and(|plan| expected.contains(&plan)))
        );
    }
}
