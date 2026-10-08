use super::*;
use serde_json::{Value, json};

fn recent_tasks() -> Value {
    json!({ "op":"tasks", "plan":"P7", "column":"done", "order":"recent_first",
        "before":null, "after":null, "ceiling":null, "through":null, "limit":20 })
}

fn rejected(value: Value, prefix: &str) {
    let operation: BoardOp = serde_json::from_value(value).unwrap();
    assert!(
        operation
            .validate()
            .unwrap_err()
            .to_string()
            .starts_with(prefix)
    );
}

#[test]
fn done_task_cursors_validate_order_and_plan_conflicts() {
    for (field, value) in [
        ("after", json!("P7.1")),
        ("ceiling", json!({"plan":"P7","ordinal":3})),
        ("through", json!(8)),
    ] {
        let mut op = recent_tasks();
        op[field] = value;
        rejected(op, "invalid_options:");
    }
    let mut ordinal = recent_tasks();
    ordinal["order"] = json!("ordinal");
    ordinal["before"] = json!({"seq":8,"id":"P7.1"});
    rejected(ordinal, "invalid_options:");
    let mut foreign = recent_tasks();
    foreign["before"] = json!({"seq":8,"id":"P8.1"});
    rejected(foreign, "invalid_reference:");
    let mut invalid_seq = recent_tasks();
    invalid_seq["before"] = json!({"seq":0,"id":"P7.1"});
    rejected(invalid_seq, "invalid_reference:");
}

#[test]
fn done_task_reads_are_query_only_and_bounded() {
    for mut value in [
        recent_tasks(),
        json!({"op":"done_tasks","scope":"all","before":null,"limit":20}),
    ] {
        let operation: BoardOp = serde_json::from_value(value.clone()).unwrap();
        assert!(operation.is_read_only());
        assert!(!operation.registers_workspace());
        operation.validate().unwrap();
        for limit in [0, 201] {
            value["limit"] = json!(limit);
            rejected(value.clone(), "invalid_options:");
        }
    }
    rejected(
        json!({"op":"done_tasks","scope":"all","before":{"seq":0,"id":"P1.1"},"limit":1}),
        "invalid_reference:",
    );
}
