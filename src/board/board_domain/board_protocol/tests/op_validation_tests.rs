use super::*;

#[test]
fn internal_entry_kinds_and_open_feedback_cannot_be_posted_or_closed() {
    let post = BoardOp::Post {
        target: BoardRef::Plan(PlanId::new(1).unwrap()),
        kind: EntryKind::Claim,
        body: EntryText::new("scope").unwrap(),
        to: None,
        supersedes: None,
    };
    assert!(
        post.validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_kind:")
    );
    let close = BoardOp::FeedbackClose {
        entry: EntryId::new(1).unwrap(),
        state: FeedbackState::Open,
        note: None,
    };
    assert!(
        close
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_state:")
    );
}

#[test]
fn claim_scope_is_required_unless_resuming() {
    for (resume, valid) in [
        (serde_json::json!(false), false),
        (serde_json::json!(true), true),
        (serde_json::json!("E42"), true),
    ] {
        let op: BoardOp = serde_json::from_value(serde_json::json!({
            "op": "claim_task", "task": "P1.1", "scope": null, "resume": resume
        }))
        .unwrap();
        assert_eq!(op.validate().is_ok(), valid, "{resume}");
    }
}

#[test]
fn claim_delegate_defaults_absent_so_older_senders_still_parse() {
    let op: BoardOp = serde_json::from_value(serde_json::json!({
        "op": "claim_task", "task": "P1.1", "scope": "lane", "resume": false
    }))
    .unwrap();
    assert!(matches!(op, BoardOp::ClaimTask { delegate: None, .. }));
    assert!(op.validate().is_ok());
    assert!(
        !serde_json::to_value(&op)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("delegate")
    );
    let delegated: BoardOp = serde_json::from_value(serde_json::json!({
        "op": "claim_task", "task": "P1.1", "scope": "lane", "resume": false,
        "delegate": {"harness": "codex", "session": "c7"}
    }))
    .unwrap();
    let BoardOp::ClaimTask {
        delegate: Some(delegate),
        ..
    } = &delegated
    else {
        panic!("expected a delegate: {delegated:?}")
    };
    assert_eq!(delegate.harness.as_str(), "codex");
    assert_eq!(delegate.session, "c7");
    assert!(delegated.validate().is_ok());
}

#[test]
fn claim_resume_entry_target_round_trips_the_wire_form() {
    let op: BoardOp = serde_json::from_value(serde_json::json!({
        "op": "claim_task", "task": "P1.1", "scope": null, "resume": "E42"
    }))
    .unwrap();
    let BoardOp::ClaimTask {
        resume: ClaimResume::Entry(entry),
        ..
    } = op
    else {
        panic!("expected an explicit entry resume: {op:?}")
    };
    assert_eq!(entry, EntryId::new(42).unwrap());
    assert_eq!(serde_json::to_value(&op).unwrap()["resume"], "E42");
    let idle: BoardOp = serde_json::from_value(serde_json::json!({
        "op": "claim_task", "task": "P1.1", "scope": null, "resume": true
    }))
    .unwrap();
    assert!(matches!(
        idle,
        BoardOp::ClaimTask {
            resume: ClaimResume::Idle,
            ..
        }
    ));
}

#[test]
fn public_address_records_are_revalidated_at_the_request_boundary() {
    let plan = PlanId::new(1).unwrap();
    let bad_revision = BoardOp::Review {
        base: PlanRevision { plan, revision: 0 },
        agent: None,
    };
    let bad_task = BoardOp::ClaimTask {
        task: TaskId {
            plan,
            ordinal: u64::MAX,
        },
        scope: Some(EntryText::new("owned scope").unwrap()),
        resume: ClaimResume::No,
        delegate: None,
    };
    assert!(bad_revision.validate().is_err());
    assert!(bad_task.validate().is_err());
}

#[test]
fn feedback_enforces_recent_call_and_metadata_bounds() {
    let call = RecentCall {
        verb: "search".into(),
        args: vec!["symbol".into()],
        exit_code: Some(2),
        error_prefix: Some("unavailable".into()),
        truncated: None,
        coverage: None,
    };
    let mut metadata = FeedbackMetadata {
        recent_calls: vec![call; 6],
        ..FeedbackMetadata::default()
    };
    assert!(metadata.validate().is_err());
    metadata.recent_calls.truncate(5);
    metadata.validate().unwrap();
    metadata.build_id = Some("x".repeat(257));
    assert!(metadata.validate().is_err());
    metadata.build_id = None;
    metadata.recent_calls[0].args = vec!["x".repeat(2048)];
    assert!(metadata.validate().is_err());
}

#[test]
fn link_commit_wire_carries_only_the_oid_and_task() {
    let oid = "a".repeat(40);
    let op: BoardOp = serde_json::from_value(serde_json::json!({
        "op": "link_commit", "oid": oid, "task": "P7.3"
    }))
    .unwrap();
    let BoardOp::LinkCommit {
        oid: parsed,
        task,
        resolution,
    } = &op
    else {
        panic!("expected a commit link: {op:?}")
    };
    assert_eq!(parsed.as_str(), oid);
    assert_eq!(task.to_string(), "P7.3");
    assert!(resolution.is_none());
    op.validate().unwrap();
    assert_eq!(
        serde_json::to_value(&op).unwrap(),
        serde_json::json!({"op": "link_commit", "oid": oid, "task": "P7.3"}),
        "the host resolution never leaves the process"
    );
    assert!(
        serde_json::from_value::<BoardOp>(serde_json::json!({
            "op": "link_commit", "oid": oid, "task": "P7.3", "resolution": null
        }))
        .is_err(),
        "clients cannot inject a forged resolution"
    );
}
