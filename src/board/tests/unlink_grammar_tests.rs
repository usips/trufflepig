use super::*;

#[test]
fn unlink_grammar_and_wire_carry_only_a_full_oid_and_task() {
    let oid = "0123456789abcdef0123456789abcdef01234567";
    let args = ["board", "unlink", oid, "P7.3"].map(str::to_owned);
    let options = cli::parse(&args).unwrap();
    let board_grammar::BoardCommand::Op(op) = board_grammar::parse(&options, None).unwrap() else {
        panic!("expected board operation")
    };
    let expected = serde_json::json!({"op": "unlink_commit", "oid": oid, "task": "P7.3"});
    assert_eq!(serde_json::to_value(&op).unwrap(), expected);
    assert_eq!(
        serde_json::from_value::<crate::board::BoardOp>(expected).unwrap(),
        op
    );
    assert_eq!(op.plan_id().unwrap().to_string(), "P7");
    assert!(!op.is_read_only());
    assert!(!op.registers_workspace());
    let forged = serde_json::json!({
        "op": "unlink_commit", "oid": oid, "task": "P7.3", "repo_key": oid,
    });
    assert!(serde_json::from_value::<crate::board::BoardOp>(forged).is_err());
}

#[test]
fn unlink_grammar_rejects_invalid_addresses_and_cross_verb_flags_before_body_reads() {
    let oid = "0123456789abcdef0123456789abcdef01234567";
    for words in [
        vec!["board", "unlink", "0123456", "P7.3"],
        vec!["board", "unlink", "invalid", "P7.3"],
        vec!["board", "unlink", oid, "P7"],
        vec!["board", "unlink", oid, "P7@3"],
        vec!["board", "unlink", oid, "P7.0"],
        vec!["board", "unlink", oid, "P7.3", "extra"],
        vec!["board", "unlink", oid, "P7.3", "--to", "codex"],
        vec!["board", "unlink", oid, "P7.3", "--body", "-"],
        vec!["board", "unlink", oid, "P7.3", "--board-text=extra"],
    ] {
        let args = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        let options = cli::parse(&args).unwrap();
        assert!(
            board_grammar::validate_before_body(&options).is_err(),
            "{words:?}"
        );
    }
}
