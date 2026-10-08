use super::*;
use crate::board::board_ids::{PlanId, RepoKey};
use crate::board::board_protocol::ReadScope;
use crate::board::board_vocabulary::{PlanText, PlanTitle};
use std::path::PathBuf;

fn registration() -> RepoRegistration {
    RepoRegistration {
        repo_key: RepoKey::parse(&"a".repeat(40)).unwrap(),
        origin_label: None,
        host: "laptop".into(),
        common_dir: PathBuf::from("/repo/.git"),
        plan_id: None,
        root_commits: Vec::new(),
        registration_error: None,
        origin_override: None,
    }
}

fn scoped_key(op: &BoardOp) -> Option<&RepoKey> {
    let scope = match op {
        BoardOp::Inbox { scope, .. }
        | BoardOp::Attention { scope, .. }
        | BoardOp::Overview { scope, .. }
        | BoardOp::Claims { scope, .. } => scope,
        _ => return None,
    };
    match scope {
        ReadScope::Repo(key) => Some(key),
        _ => None,
    }
}

#[test]
fn scoped_reads_fill_the_caller_repository_and_all_stays_global() {
    let registration = registration();
    let expected = Some(registration.repo_key.clone());
    let mut scoped = [
        BoardOp::Inbox {
            scope: ReadScope::All,
            after: None,
            limit: 20,
        },
        BoardOp::Attention {
            scope: ReadScope::All,
            after: None,
            through: None,
            limit: 20,
        },
        BoardOp::Overview {
            scope: ReadScope::All,
            after: None,
            through: None,
            limit: 20,
        },
        BoardOp::Claims {
            scope: ReadScope::All,
            plan: None,
            own_stale: true,
            after: None,
            through: None,
            limit: 20,
        },
    ];
    for op in &mut scoped {
        scope_read_repo_key(op, Some(&registration), false);
        assert_eq!(
            scoped_key(op),
            expected.as_ref(),
            "unexpected scope for {op:?}"
        );
    }
    let mut global = [
        BoardOp::Inbox {
            scope: ReadScope::All,
            after: None,
            limit: 20,
        },
        BoardOp::Attention {
            scope: ReadScope::All,
            after: None,
            through: None,
            limit: 20,
        },
        BoardOp::Overview {
            scope: ReadScope::All,
            after: None,
            through: None,
            limit: 20,
        },
        BoardOp::Claims {
            scope: ReadScope::All,
            plan: None,
            own_stale: true,
            after: None,
            through: None,
            limit: 20,
        },
    ];
    for op in &mut global {
        scope_read_repo_key(op, Some(&registration), true);
        assert_eq!(scoped_key(op), None, "--all must stay global for {op:?}");
    }
    let mut write = BoardOp::New {
        title: PlanTitle::new("writes keep their own repo link").unwrap(),
        body: PlanText::new("").unwrap(),
        steward: None,
        repo_key: None,
    };
    scope_read_repo_key(&mut write, Some(&registration), false);
    assert!(
        matches!(write, BoardOp::New { repo_key: None, .. }),
        "writes never receive a read scope"
    );
    let mut inbox = BoardOp::Inbox {
        scope: ReadScope::All,
        after: None,
        limit: 20,
    };
    scope_read_repo_key(&mut inbox, None, false);
    assert_eq!(scoped_key(&inbox), None);
}

#[test]
fn host_preserves_regular_plan_claims_read_scope() {
    let registration = registration();
    let plan = PlanId::new(7).unwrap();
    for scope in [
        ReadScope::All,
        ReadScope::Repo(registration.repo_key.clone()),
        ReadScope::Keys(std::collections::BTreeSet::from([registration
            .repo_key
            .clone()])),
        ReadScope::Unscoped,
    ] {
        let mut op = BoardOp::Claims {
            scope: scope.clone(),
            plan: Some(plan),
            own_stale: false,
            after: None,
            through: None,
            limit: 20,
        };
        scope_read_repo_key(&mut op, Some(&registration), false);
        let BoardOp::Claims { scope: actual, .. } = op else {
            panic!("claims operation");
        };
        assert_eq!(actual, scope);
    }
}
