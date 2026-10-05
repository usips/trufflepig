use super::*;
use crate::board::board_ids::RepoKey;
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
    match op {
        BoardOp::Inbox { repo_key, .. }
        | BoardOp::Attention { repo_key, .. }
        | BoardOp::Overview { repo_key, .. }
        | BoardOp::Claims { repo_key, .. } => repo_key.as_ref(),
        _ => None,
    }
}

#[test]
fn scoped_reads_fill_the_caller_repository_and_all_stays_global() {
    let registration = registration();
    let expected = Some(registration.repo_key.clone());
    let mut scoped = [
        BoardOp::Inbox {
            after: None,
            limit: 20,
            repo_key: None,
            all: false,
        },
        BoardOp::Attention {
            repo_key: None,
            all: false,
            after: None,
            through: None,
            limit: 20,
        },
        BoardOp::Overview {
            repo_key: None,
            all: false,
            after: None,
            through: None,
            limit: 20,
        },
        BoardOp::Claims {
            plan: None,
            own_stale: true,
            repo_key: None,
            all: false,
            after: None,
            through: None,
            limit: 20,
        },
    ];
    for op in &mut scoped {
        scope_read_repo_key(op, Some(&registration));
        assert_eq!(scoped_key(op), expected.as_ref(), "unexpected scope for {op:?}");
    }
    let mut global = [
        BoardOp::Inbox {
            after: None,
            limit: 20,
            repo_key: None,
            all: true,
        },
        BoardOp::Attention {
            repo_key: None,
            all: true,
            after: None,
            through: None,
            limit: 20,
        },
        BoardOp::Overview {
            repo_key: None,
            all: true,
            after: None,
            through: None,
            limit: 20,
        },
        BoardOp::Claims {
            plan: None,
            own_stale: true,
            repo_key: None,
            all: true,
            after: None,
            through: None,
            limit: 20,
        },
    ];
    for op in &mut global {
        scope_read_repo_key(op, Some(&registration));
        assert_eq!(scoped_key(op), None, "--all must stay global for {op:?}");
    }
    let mut write = BoardOp::New {
        title: PlanTitle::new("writes keep their own repo link").unwrap(),
        body: PlanText::new("").unwrap(),
        steward: None,
        repo_key: None,
    };
    scope_read_repo_key(&mut write, Some(&registration));
    assert!(
        matches!(write, BoardOp::New { repo_key: None, .. }),
        "writes never receive a read scope"
    );
    let mut inbox = BoardOp::Inbox {
        after: None,
        limit: 20,
        repo_key: None,
        all: false,
    };
    scope_read_repo_key(&mut inbox, None);
    assert_eq!(scoped_key(&inbox), None);
}
