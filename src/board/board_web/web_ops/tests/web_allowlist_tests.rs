use super::*;
use std::net::{IpAddr, Ipv4Addr};

fn peer_ip() -> IpAddr {
    Ipv4Addr::LOCALHOST.into()
}

#[test]
fn lifecycle_and_transfer_use_captured_web_identity_and_typed_validation() {
    let config = BoardConfig::for_database("target/web-op.sqlite3");
    for op in [
        serde_json::json!({"op":"task_transfer","task":"P1.2","destination":"P3","note":"Move the trail"}),
        serde_json::json!({"op":"plan_close","plan":"P1","note":"Work completed"}),
        serde_json::json!({"op":"plan_reopen","plan":"P1","note":"Follow-up work"}),
    ] {
        let input = serde_json::json!({"api":BOARD_API,"op":op});
        let request = serde_json::from_value::<WebRequest>(input)
            .unwrap()
            .into_request(&config, peer_ip())
            .unwrap();
        assert_eq!(request.actor.identity(), "test@localhost/human/web");
        assert!(request.claims.is_none());
        assert!(!request.op.is_read_only());
    }
    let invalid = serde_json::json!({"api":BOARD_API,"op":{
        "op":"task_transfer","task":"P1.2","destination":"P1","note":"Same plan"
    }});
    let error = serde_json::from_value::<WebRequest>(invalid)
        .unwrap()
        .into_request(&config, peer_ip())
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidReference);
}

#[test]
fn web_request_rejects_client_identity_and_agent_claims() {
    for field in ["actor", "claims", "model", "effort", "origin", "peer_ip"] {
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
        project: None,
        op: BoardOp::Show {
            target: crate::board::board_ids::BoardRef::parse("P1").unwrap(),
        },
    }
    .into_request(&config, peer_ip())
    .unwrap();
    assert_eq!(request.actor.identity(), "test@localhost/human/web");
    assert_eq!(request.origin.unwrap().peer_ip, Some(peer_ip()));
    assert!(request.claims.is_none());
    let rejected = WebRequest {
        api: BOARD_API,
        project: None,
        op: BoardOp::Hello {
            model: "forged".into(),
            effort: None,
            worker: None,
        },
    };
    assert!(rejected.into_request(&config, peer_ip()).is_err());
}

#[test]
fn review_is_refused_over_the_web_allowlist() {
    let config = BoardConfig::for_database("target/web-op.sqlite3");
    let request = WebRequest {
        api: BOARD_API,
        project: None,
        op: BoardOp::Review {
            base: crate::board::board_ids::PlanRevision::new(
                crate::board::board_ids::PlanId::new(1).unwrap(),
                1,
            )
            .unwrap(),
            agent: None,
        },
    };
    let error = request.into_request(&config, peer_ip()).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(error.message, "op not available over web");
}

#[test]
fn link_commit_is_refused_over_the_web_allowlist() {
    let config = BoardConfig::for_database("target/web-op.sqlite3");
    let request = WebRequest {
        api: BOARD_API,
        project: None,
        op: BoardOp::LinkCommit {
            oid: tp_core::identity::GitOid::parse(&"a".repeat(40)).unwrap(),
            task: crate::board::board_ids::TaskId::new(
                crate::board::board_ids::PlanId::new(1).unwrap(),
                1,
            )
            .unwrap(),
            resolution: None,
        },
    };
    let error = request.into_request(&config, peer_ip()).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(error.message, "op not available over web");
}
