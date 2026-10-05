use super::*;

#[test]
fn web_request_rejects_client_identity_and_agent_claims() {
    for field in ["actor", "claims", "model", "effort"] {
        let mut input =
            serde_json::json!({ "api": BOARD_API, "op": { "op": "show", "target": "P1" } });
        input[field] = serde_json::json!({});
        assert!(serde_json::from_value::<WebRequest>(input).is_err());
    }
}

#[test]
fn web_identity_is_local_human_and_unspoofable() {
    let config = BoardConfig::for_database("target/web-op.sqlite3");
    let request = WebRequest {
        api: BOARD_API,
        op: BoardOp::Show {
            target: crate::board::board_ids::BoardRef::parse("P1").unwrap(),
        },
    }
    .into_request(&config)
    .unwrap();
    assert_eq!(request.actor.identity(), "test@localhost/human/web");
    assert!(request.claims.is_none());
    let rejected = WebRequest {
        api: BOARD_API,
        op: BoardOp::Hello {
            model: "forged".into(),
            effort: None,
        },
    };
    assert!(rejected.into_request(&config).is_err());
}

#[test]
fn review_is_refused_over_the_web_allowlist() {
    let config = BoardConfig::for_database("target/web-op.sqlite3");
    let request = WebRequest {
        api: BOARD_API,
        op: BoardOp::Review {
            base: crate::board::board_ids::PlanRevision::new(
                crate::board::board_ids::PlanId::new(1).unwrap(),
                1,
            )
            .unwrap(),
            agent: None,
        },
    };
    let error = request.into_request(&config).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(error.message, "op not available over web");
}

#[test]
fn link_commit_is_refused_over_the_web_allowlist() {
    let config = BoardConfig::for_database("target/web-op.sqlite3");
    let request = WebRequest {
        api: BOARD_API,
        op: BoardOp::LinkCommit {
            oid: crate::identity::GitOid::parse(&"a".repeat(40)).unwrap(),
            task: crate::board::board_ids::TaskId::new(
                crate::board::board_ids::PlanId::new(1).unwrap(),
                1,
            )
            .unwrap(),
            resolution: None,
        },
    };
    let error = request.into_request(&config).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(error.message, "op not available over web");
}
