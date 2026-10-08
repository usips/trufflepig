mod scope_read_fixture;

use super::*;
use scope_read_fixture::ScopeFixture;
use serde_json::json;

#[test]
fn keys_scope_shows_plans_of_any_member_repo_only() {
    let mut fixture = ScopeFixture::new();
    let admitted = [fixture.plans[0], fixture.plans[1]];
    fixture.assert_reads(
        json!({"keys":[fixture.keys[0], fixture.keys[1]]}),
        &admitted,
    );
    fixture.assert_reads(json!({"keys":[]}), &[]);
    let scope = json!({"keys":[fixture.keys[0], fixture.keys[1]]});
    let foreign = fixture.plans[2];
    let claims = fixture.read(
        scope.clone(),
        json!({
            "op":"claims", "plan":foreign, "own_stale":false,
            "after":null, "through":null, "limit":100,
        }),
    );
    let BoardResult::Claims(claims) = claims.result else {
        panic!("claims result");
    };
    assert!(
        claims.claims.is_empty(),
        "an explicit plan cannot bypass project scope"
    );
    let feed = fixture.read(
        scope,
        json!({
            "op":"feed", "plan":foreign, "after":null, "through":null, "limit":100,
        }),
    );
    let BoardResult::Feed(feed) = feed.result else {
        panic!("feed result");
    };
    assert!(
        feed.events.is_empty(),
        "a plan feed cannot bypass project scope"
    );
}

#[test]
fn unscoped_scope_shows_only_unlinked_plans() {
    let mut fixture = ScopeFixture::new();
    let admitted = [fixture.plans[3]];
    fixture.assert_reads(json!("unscoped"), &admitted);
}

#[test]
fn claims_repo_scope_intersects_explicit_plan() {
    let mut fixture = ScopeFixture::new();
    let scope = json!({"repo":fixture.keys[0]});
    for (plan, admitted) in [
        (fixture.plans[0], true),
        (fixture.plans[2], false),
        (fixture.plans[3], true),
    ] {
        let reply = fixture.read(
            scope.clone(),
            json!({
                "op":"claims", "plan":plan, "own_stale":false, "after":null,
                "through":null, "limit":100,
            }),
        );
        let BoardResult::Claims(claims) = reply.result else {
            panic!("claims result");
        };
        assert_eq!(
            !claims.claims.is_empty(),
            admitted,
            "plan {plan} must intersect Repo scope"
        );
        assert!(claims.claims.iter().all(|row| row.claim.task.plan == plan));
    }
}

#[test]
fn plan_in_two_projects_appears_in_both() {
    let mut fixture = ScopeFixture::new();
    let shared = fixture.plans[0];
    fixture
        .board
        .conn
        .execute(
            "INSERT INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
            params![sql_number(shared.get()), fixture.keys[2].as_str()],
        )
        .unwrap();
    for key in [fixture.keys[0].clone(), fixture.keys[2].clone()] {
        let reply = fixture.read(
            json!({"keys":[key]}),
            json!({
                "op":"overview", "after":null, "through":null, "limit":100,
            }),
        );
        let BoardResult::Overview(overview) = reply.result else {
            panic!("overview result");
        };
        assert!(overview.plans.iter().any(|row| row.plan.id == shared));
        let wire = serde_json::to_value(overview).unwrap();
        let row = wire["plans"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["plan"]["id"] == json!(shared))
            .unwrap();
        assert_eq!(row["repo_keys"].as_array().unwrap().len(), 2);
    }
}

#[test]
fn repo_scope_retains_addressed_messages_and_own_feedback() {
    let mut fixture = ScopeFixture::new();
    let scope = json!({"repo":fixture.keys[0]});
    let reply = fixture.read(
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
            .collect::<Vec<_>>(),
        vec![fixture.plans[0], fixture.plans[3]]
    );
    let reply = fixture.read(
        scope.clone(),
        json!({
            "op":"attention", "after":null, "through":null, "limit":100,
        }),
    );
    let BoardResult::Attention(attention) = reply.result else {
        panic!("attention result");
    };
    assert!(
        attention
            .entries
            .iter()
            .any(|row| row.plan == Some(fixture.plans[2]))
    );
    assert!(attention.entries.iter().any(|row| row.plan.is_none()));
    let reply = fixture.read(scope, json!({"op":"inbox", "after":0, "limit":100}));
    let BoardResult::Inbox(inbox) = reply.result else {
        panic!("inbox result");
    };
    assert!(
        inbox
            .events
            .iter()
            .any(|row| row.plan == Some(fixture.plans[2]))
    );
    assert!(inbox.events.iter().any(|row| row.plan.is_none()));
    assert!(
        inbox
            .open
            .iter()
            .any(|row| row.plan == Some(fixture.plans[2]))
    );
}

#[test]
fn shared_commit_event_respects_requested_plan_scope_intersection() {
    use crate::board::board_protocol::{CommitPlanLink, LinkedCommit};
    let mut fixture = ScopeFixture::new();
    let start = fixture.board.max_seq().unwrap();
    let commits = [0, 2]
        .map(|index| LinkedCommit {
            repo_key: fixture.keys[index].clone(),
            oid: crate::identity::GitOid::parse(&format!("{:040x}", index + 8)).unwrap(),
            subject: "shared batch event".into(),
            committed_at: 10,
            author: "Josh".into(),
            coauthors: Vec::new(),
            files: 1,
            insertions: 1,
            deletions: 0,
            plans: vec![CommitPlanLink {
                plan_id: fixture.plans[index],
                task_ordinal: None,
            }],
        })
        .into_iter()
        .collect();
    fixture
        .board
        .handle(&BoardRequest::new(
            actor("human", "owner"),
            BoardOp::LinkCommits { commits },
        ))
        .unwrap();
    let scope = json!({"keys":[fixture.keys[0]]});
    for (plan, count) in [
        (None, 1),
        (Some(fixture.plans[0]), 1),
        (Some(fixture.plans[2]), 0),
    ] {
        let reply = fixture.read(
            scope.clone(),
            json!({"op":"feed", "plan":plan, "after":start, "through":null, "limit":100}),
        );
        let BoardResult::Feed(feed) = reply.result else {
            panic!("feed result");
        };
        assert_eq!(feed.events.len(), count, "requested plan {plan:?}");
    }
}

#[test]
fn plan_view_and_overview_keep_repository_keys_after_checkout_paths_are_removed() {
    let mut fixture = ScopeFixture::new();
    let plan = fixture.plans[0];
    let key = fixture.keys[0].clone();
    fixture.board.conn.execute(
        "INSERT INTO repo_paths(repo_key,host,common_dir,root_commits_json) VALUES(?1,'fixture-host','/missing/.git','[]')",
        [key.as_str()],
    ).unwrap();
    fixture
        .board
        .conn
        .execute("DELETE FROM repo_paths", [])
        .unwrap();
    let reply = fixture
        .board
        .handle(&BoardRequest::new(
            actor("human", "owner"),
            BoardOp::Show {
                target: BoardRef::Plan(plan),
            },
        ))
        .unwrap();
    let wire = serde_json::to_value(&reply).unwrap();
    assert_eq!(wire["result"]["data"]["repo_keys"], json!([key]));
    let reply = fixture.read(
        json!({"keys":[key]}),
        json!({
            "op":"overview", "after":null, "through":null, "limit":100,
        }),
    );
    let BoardResult::Overview(overview) = reply.result else {
        panic!("overview result");
    };
    assert_eq!(overview.plans.len(), 1);
    assert_eq!(overview.plans[0].repo_keys, vec![key]);
}
