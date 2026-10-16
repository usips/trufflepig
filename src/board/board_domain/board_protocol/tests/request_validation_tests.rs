use super::*;
use crate::board::board_protocol::{ClaimDelegate, ReadScope};

#[test]
fn claim_delegates_refuse_reserved_harnesses() {
    for value in [
        "human/owner",
        "cli/owner",
        "system/owner",
        "git:dev@example.invalid/owner",
    ] {
        let error = ClaimDelegate::parse(value).unwrap_err().to_string();
        assert!(error.starts_with("invalid_options:"), "{value}: {error}");
    }
    let delegate = ClaimDelegate::parse("codex/c7").unwrap();
    assert_eq!(delegate.harness.as_str(), "codex");
    assert_eq!(delegate.session, "c7");
}

#[test]
fn git_coauthor_actor_round_trips_as_evidence_but_cannot_request() {
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("git:alice@example.com").unwrap(),
        "git-evidence",
    )
    .unwrap();
    let encoded = serde_json::to_string(&actor).unwrap();
    assert_eq!(serde_json::from_str::<BoardActor>(&encoded).unwrap(), actor);
    let request = BoardRequest::new(
        actor,
        BoardOp::Show {
            target: BoardRef::Plan(PlanId::new(1).unwrap()),
        },
    );
    assert!(
        request
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_actor:")
    );
}

#[test]
fn versioned_requests_round_trip_and_reject_mismatch() {
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "c1",
    )
    .unwrap();
    let mut request = BoardRequest::new(
        actor,
        BoardOp::Inbox {
            scope: ReadScope::All,
            after: Some(EventSeq::new(0)),
            limit: 20,
        },
    );
    let decoded: BoardRequest =
        serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
    assert_eq!(decoded, request);
    decoded.validate().unwrap();
    request.api += 1;
    assert!(
        request
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("board_api_mismatch:")
    );
}

#[test]
fn trusted_internal_fields_are_never_serialized_and_system_origin_is_not_client_input() {
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "c1",
    )
    .unwrap();
    let mut request = BoardRequest::new(
        actor,
        BoardOp::Show {
            target: BoardRef::Plan(PlanId::new(1).unwrap()),
        },
    );
    request.origin = Some(ActorOrigin {
        harness: HarnessLabel::parse("system").unwrap(),
        session: "maintenance".into(),
        peer_ip: None,
    });
    assert!(request.validate().is_err());

    let mut request = BoardRequest::new(
        BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse("codex").unwrap(),
            "c1",
        )
        .unwrap(),
        BoardOp::Bye,
    );
    request.trusted_internal = true;
    let encoded = serde_json::to_value(&request).unwrap();
    assert!(
        !encoded
            .as_object()
            .unwrap()
            .contains_key("trusted_internal")
    );
    assert!(!encoded.as_object().unwrap().contains_key("trusted_head"));
    assert!(
        serde_json::from_value::<BoardRequest>(serde_json::json!({
            "api": 10,
            "actor": encoded["actor"],
            "op": {"op": "bye"},
            "trusted_internal": true
        }))
        .is_err()
    );
}

#[test]
fn request_peer_ip_requires_server_observation_and_observation_is_not_serialized() {
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "c1",
    )
    .unwrap();
    let mut request = BoardRequest::new(
        actor,
        BoardOp::Show {
            target: BoardRef::Plan(PlanId::new(1).unwrap()),
        },
    );
    let peer_ip = std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 0, 42));
    request.origin = Some(ActorOrigin::from_actor(&request.actor));
    request.origin.as_mut().unwrap().peer_ip = Some(peer_ip);
    assert!(
        request
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_origin:")
    );

    request.observe_peer_ip(peer_ip);
    request.validate().unwrap();
    request.origin.as_mut().unwrap().peer_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(
        192, 168, 0, 43,
    )));
    assert!(
        request
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_origin:")
    );
    request.observe_peer_ip(peer_ip);
    let encoded = serde_json::to_value(&request).unwrap();
    assert!(
        !encoded
            .as_object()
            .unwrap()
            .contains_key("observed_peer_ip")
    );
    let decoded: BoardRequest = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.origin.as_ref().unwrap().peer_ip, Some(peer_ip));
    assert!(
        decoded
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_origin:")
    );
}
