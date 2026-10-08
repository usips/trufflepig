use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn read_contract_rejects_missing_show_target_and_invalid_claim_cursors() {
    assert!(serde_json::from_value::<BoardOp>(serde_json::json!({"op":"show"})).is_err());
    assert!(
        serde_json::from_value::<BoardOp>(serde_json::json!({"op":"show","target":null})).is_err()
    );
    for claim in [0, crate::board::board_ids::MAX_BOARD_NUMBER + 1] {
        let operation = BoardOp::Claims {
            scope: ReadScope::All,
            plan: Some(PlanId::new(1).unwrap()),
            own_stale: false,
            after: Some(ClaimCursor {
                entry: EntryId::new(1).unwrap(),
                claim,
            }),
            through: None,
            limit: 1,
        };
        assert!(operation.is_read_only());
        assert!(
            operation
                .validate()
                .unwrap_err()
                .to_string()
                .starts_with("invalid_reference:")
        );
    }
}

#[test]
fn task_read_contract_validates_ceilings_and_requires_captured_membership() {
    let plan = PlanId::new(1).unwrap();
    let operation = |after, ceiling, through| BoardOp::Tasks {
        column: None,
        order: TaskOrder::Ordinal,
        before: None,
        plan,
        after,
        ceiling,
        through,
        limit: 1,
    };
    let empty = TaskCeiling { plan, ordinal: 0 };
    operation(None, Some(empty), Some(EventSeq::new(10)))
        .validate()
        .unwrap();
    let decoded: BoardOp = serde_json::from_value(serde_json::json!({
        "op": "tasks", "order": "ordinal", "plan": "P1", "after": null,
        "ceiling": {"plan": "P1", "ordinal": 0}, "through": 10, "limit": 1
    }))
    .unwrap();
    assert_eq!(
        decoded,
        operation(None, Some(empty), Some(EventSeq::new(10)))
    );
    for ceiling in [
        TaskCeiling {
            plan: PlanId::new(2).unwrap(),
            ordinal: 1,
        },
        TaskCeiling {
            plan,
            ordinal: crate::board::board_ids::MAX_BOARD_NUMBER + 1,
        },
    ] {
        assert!(
            operation(None, Some(ceiling), None)
                .validate()
                .unwrap_err()
                .to_string()
                .starts_with("invalid_reference:")
        );
    }
    for operation in [
        operation(Some(TaskId::new(plan, 1).unwrap()), None, None),
        operation(None, None, Some(EventSeq::new(10))),
    ] {
        assert!(
            operation
                .validate()
                .unwrap_err()
                .to_string()
                .starts_with("invalid_options:")
        );
    }
}

#[test]
fn collection_operations_share_read_classification_and_bounds() {
    let cases = [
        (
            serde_json::json!({"op": "overview", "scope":"all", "limit": 1}),
            None,
            200,
        ),
        (
            serde_json::json!({"op": "attention", "scope":"all", "limit": 1}),
            None,
            200,
        ),
        (
            serde_json::json!({"op": "feed", "scope":"all", "plan": "P1", "limit": 1}),
            Some("P1"),
            500,
        ),
        (
            serde_json::json!({"op": "history", "plan": "P1", "limit": 1}),
            Some("P1"),
            200,
        ),
        (
            serde_json::json!({"op": "entries", "plan": "P1", "limit": 1}),
            Some("P1"),
            200,
        ),
        (
            serde_json::json!({"op": "tasks", "order": "ordinal", "plan": "P1", "limit": 1}),
            Some("P1"),
            200,
        ),
        (
            serde_json::json!({"op": "claims", "plan": "P1", "own_stale": false,
            "scope":"all", "limit": 1}),
            Some("P1"),
            200,
        ),
        (
            serde_json::json!({"op": "feedback_list", "open_only": true, "limit": 1}),
            None,
            200,
        ),
    ];
    for (mut encoded, expected_plan, maximum) in cases {
        let operation: BoardOp = serde_json::from_value(encoded.clone()).unwrap();
        assert!(operation.is_read_only());
        operation.validate().unwrap();
        assert_eq!(
            operation.plan_id().map(|id| id.to_string()).as_deref(),
            expected_plan
        );
        encoded["limit"] = maximum.into();
        serde_json::from_value::<BoardOp>(encoded.clone())
            .unwrap()
            .validate()
            .unwrap();
        for limit in [0, maximum + 1] {
            encoded["limit"] = limit.into();
            let error = serde_json::from_value::<BoardOp>(encoded.clone())
                .unwrap()
                .validate()
                .unwrap_err();
            assert!(error.to_string().starts_with("invalid_options:"));
        }
    }
    for encoded in [
        serde_json::json!({"op": "show", "target": "P1"}),
        serde_json::json!({"op": "review", "base": "P1@1"}),
        serde_json::json!({"op": "repositories", "plan": "P1"}),
        serde_json::json!({"op": "inbox", "after": 0, "limit": 1, "scope":"all"}),
    ] {
        let operation: BoardOp = serde_json::from_value(encoded).unwrap();
        assert!(operation.is_read_only());
        operation.validate().unwrap();
    }
    let inbox = BoardOp::Inbox {
        scope: ReadScope::All,
        after: None,
        limit: 1,
    };
    assert!(!inbox.is_read_only());
}

#[test]
fn entries_rejects_combined_after_and_before_cursors() {
    let cursor = |seq: u64, entry: u64| EntryCursor {
        seq: EventSeq::new(seq),
        entry: EntryId::new(entry).unwrap(),
    };
    let operation = |after, before| BoardOp::Entries {
        plan: Some(PlanId::new(1).unwrap()),
        kind: None,
        harness: None,
        user: None,
        host: None,
        task: None,
        references: None,
        after,
        before,
        through: None,
        limit: 50,
    };
    assert!(
        operation(Some(cursor(1, 1)), Some(cursor(2, 2)))
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with("invalid_options:")
    );
    for (after, before) in [
        (Some(cursor(1, 1)), None),
        (None, Some(cursor(2, 2))),
        (None, None),
    ] {
        operation(after, before).validate().unwrap();
    }
    let decoded: BoardOp = serde_json::from_value(serde_json::json!({
        "op": "entries", "plan": "P1",
        "before": {"seq": 2, "entry": "E2"}, "limit": 50
    }))
    .unwrap();
    assert_eq!(decoded, operation(None, Some(cursor(2, 2))));
}

#[test]
fn task_continuations_reject_foreign_and_out_of_range_cursors() {
    let plan = PlanId::new(1).unwrap();
    let ceiling = TaskCeiling { plan, ordinal: 1 };
    for after in [
        TaskId::new(PlanId::new(2).unwrap(), 1).unwrap(),
        TaskId { plan, ordinal: 0 },
        TaskId {
            plan,
            ordinal: crate::board::board_ids::MAX_BOARD_NUMBER + 1,
        },
    ] {
        let operation = BoardOp::Tasks {
            column: None,
            order: TaskOrder::Ordinal,
            before: None,
            plan,
            after: Some(after),
            ceiling: Some(ceiling),
            through: None,
            limit: 1,
        };
        assert!(
            operation
                .validate()
                .unwrap_err()
                .to_string()
                .starts_with("invalid_reference:")
        );
    }
    BoardOp::Tasks {
        column: None,
        order: TaskOrder::Ordinal,
        before: None,
        plan,
        after: Some(TaskId::new(plan, 2).unwrap()),
        ceiling: Some(ceiling),
        through: None,
        limit: 1,
    }
    .validate()
    .unwrap();
    TaskCeiling {
        plan,
        ordinal: crate::board::board_ids::MAX_BOARD_NUMBER,
    }
    .validate()
    .unwrap();
    assert_eq!(
        serde_json::to_value(ceiling).unwrap(),
        serde_json::json!({"plan": "P1", "ordinal": 1})
    );
    assert!(
        serde_json::from_value::<TaskCeiling>(serde_json::json!({
            "plan": "P1", "ordinal": 1, "through": 10
        }))
        .is_err()
    );
}

#[test]
fn feedback_pages_round_trip_open_filter_and_snapshot_sequence() {
    let page = FeedbackPage {
        open_only: true,
        feedback: Vec::new(),
        after: None,
        through: EventSeq::new(10),
        next_after: None,
        omitted: 0,
    };
    let mut reply = BoardReply::new("local", BoardResult::Feedback(page));
    reply.snapshot_seq = Some(EventSeq::new(10));
    let encoded = serde_json::to_value(&reply).unwrap();
    assert_eq!(encoded["api"], BOARD_API);
    assert_eq!(encoded["snapshot_seq"], 10);
    assert_eq!(encoded["result"]["data"]["open_only"], true);
    assert_eq!(
        serde_json::from_value::<BoardReply>(encoded).unwrap(),
        reply
    );
    assert!(
        serde_json::from_value::<BoardResult>(serde_json::json!({
            "result": "plans", "data": []
        }))
        .is_err()
    );
}
