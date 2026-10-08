use super::*;
use crate::board::board_protocol::ReadScope;

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
