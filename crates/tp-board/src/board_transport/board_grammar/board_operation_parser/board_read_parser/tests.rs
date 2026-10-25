use crate::board_test_support::parse_board_arguments as parse;

#[test]
fn pending_grammar_builds_a_bounded_notice_cursor() {
    use crate::board_grammar::{BoardCommand, parse as board_parse};
    use crate::board_protocol::BoardOp;

    let args = ["board", "pending", "0"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let BoardCommand::Op(BoardOp::Pending { after, limit }) = board_parse(&options, None).unwrap()
    else {
        panic!("expected pending notices");
    };
    assert_eq!(after.unwrap().get(), 0);
    assert!((1..=200).contains(&limit));

    let args = ["board", "pending", "18446744073709551615"].map(str::to_owned);
    assert!(board_parse(&parse(&args).unwrap(), None).is_err());
}

#[test]
fn board_collection_grammar_uses_frozen_typed_cursors_and_required_show_targets() {
    use crate::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
    use crate::board_protocol::{BoardOp, EntryCursor, OverviewCursor};
    let examples = [
        vec!["board", "show"],
        vec!["board", "show", "--after=12:P7", "--through=90", "-n3"],
        vec!["board", "show", "--all"],
        vec!["board", "show", "E512"],
        vec!["board", "show", "P7.3"],
        vec!["board", "feed"],
        vec!["board", "feed", "P7", "12", "--through=90"],
        vec!["board", "feed", "12"],
        vec!["board", "feed", "P7", "--after=12", "--through=90"],
        vec![
            "board",
            "attention",
            "--all",
            "--after=12:E512",
            "--through=90",
        ],
        vec!["board", "history", "P7", "12", "--through=90"],
        vec![
            "feedback",
            "ls",
            "--open",
            "--after=12:E512",
            "--through=90",
        ],
    ];
    for words in examples {
        let args = words.iter().map(|word| (*word).into()).collect::<Vec<_>>();
        let options = parse(&args).unwrap();
        let original = board_parse(&options, None).unwrap();
        let forwarded = normalize_args(&options, None).unwrap();
        assert_eq!(
            board_parse(&crate::board_test_support::round_trip(&forwarded), None).unwrap(),
            original,
            "{words:?}"
        );
    }
    let args = ["board", "attention", "--after=12:E512", "--through=90"].map(str::to_owned);
    let BoardCommand::Op(BoardOp::Attention {
        after,
        through,
        limit,
        ..
    }) = board_parse(&parse(&args).unwrap(), None).unwrap()
    else {
        panic!("attention");
    };
    assert_eq!(
        after,
        Some(EntryCursor {
            seq: "12".parse().unwrap(),
            entry: "E512".parse().unwrap()
        })
    );
    assert_eq!(through.unwrap().get(), 90);
    assert_eq!(limit, 200);
    let bare = parse(&["board".into(), "show".into()]).unwrap();
    let BoardCommand::Op(BoardOp::Overview { limit, scope, .. }) =
        board_parse(&bare, None).unwrap()
    else {
        panic!("overview");
    };
    assert_eq!(limit, 200);
    assert!(!bare.board.all);
    assert!(scope.is_all());
    let continuation = parse(&[
        "board".into(),
        "show".into(),
        "--after=12:P7".into(),
        "--through=90".into(),
    ])
    .unwrap();
    let BoardCommand::Op(BoardOp::Overview {
        after, active_only, ..
    }) = board_parse(&continuation, None).unwrap()
    else {
        panic!("overview continuation");
    };
    assert_eq!(
        after,
        Some(OverviewCursor {
            seq: "12".parse().unwrap(),
            plan: "P7".parse().unwrap(),
        })
    );
    assert!(!active_only);
    let global = parse(&["board".into(), "show".into(), "--all".into()]).unwrap();
    let BoardCommand::Op(BoardOp::Overview { scope, .. }) = board_parse(&global, None).unwrap()
    else {
        panic!("overview --all");
    };
    assert!(global.board.all);
    assert!(scope.is_all());
}

#[test]
fn board_collection_grammar_rejects_wrong_cursor_types_and_cross_command_flags() {
    use crate::board_grammar::parse as board_parse;
    for words in [
        vec!["board", "feed", "P7", "12", "--after=13"],
        vec!["board", "feed", "12", "13"],
        vec!["board", "feed", "--after=12:E512"],
        vec!["board", "attention", "--after=12"],
        vec!["board", "attention", "--after=12:E0512"],
        vec!["board", "history", "P7@1"],
        vec!["board", "history", "P7", "--all"],
        vec!["board", "show", "P7", "--after=P1"],
        vec!["board", "show", "P7", "--all"],
        vec!["board", "show", "--after=12:E512"],
        vec!["board", "show", "-n201"],
        vec!["board", "feed", "-n501"],
        vec!["feedback", "ls", "--through=P7"],
        vec!["feedback", "ls", "--after=12:E512", "--body=-"],
        vec!["board", "attention", "--sem"],
        vec!["board", "feed", "--member=main"],
        vec!["board", "history", "P7", "--cache=source-cache"],
    ] {
        let options = parse(&words.iter().map(|word| (*word).into()).collect::<Vec<_>>()).unwrap();
        assert!(board_parse(&options, None).is_err(), "{words:?}");
    }
}

#[test]
fn receipt_grammar_requires_a_full_hexadecimal_request_key() {
    use crate::board_grammar::{BoardCommand, parse as board_parse};
    use crate::board_protocol::BoardOp;

    let key = "ab".repeat(32);
    let args = ["board", "receipt", &key].map(str::to_owned);
    let BoardCommand::Op(BoardOp::Receipt { key: parsed }) =
        board_parse(&parse(&args).unwrap(), None).unwrap()
    else {
        panic!("expected receipt lookup");
    };
    assert_eq!(parsed.to_string(), key);
    let operation = BoardOp::Receipt { key: parsed };
    assert!(operation.is_read_only());
    let encoded = serde_json::to_string(&operation).unwrap();
    assert!(encoded.contains(&format!("\"key\":\"{key}\"")));
    assert_eq!(
        serde_json::from_str::<BoardOp>(&encoded).unwrap(),
        operation
    );

    let non_hex = "z".repeat(64);
    for invalid in ["short", non_hex.as_str()] {
        let args = ["board", "receipt", invalid].map(str::to_owned);
        assert!(board_parse(&parse(&args).unwrap(), None).is_err());
    }
}

#[test]
fn board_search_round_trip_preserves_raw_query_and_plan_scope() {
    use crate::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
    use crate::board_protocol::BoardOp;
    let args = [
        "board",
        "search",
        "--board-text=\"parser scope\" OR feedback",
        "--plan=P7",
    ]
    .map(str::to_owned);
    let options = parse(&args).unwrap();
    let forwarded = normalize_args(&options, None).unwrap();
    let BoardCommand::Op(BoardOp::Search { query, plan, limit }) =
        board_parse(&crate::board_test_support::round_trip(&forwarded), None).unwrap()
    else {
        panic!("search");
    };
    assert_eq!(query, "\"parser scope\" OR feedback");
    assert_eq!(plan.unwrap().to_string(), "P7");
    assert_eq!(limit, 50);
    for words in [
        vec!["board", "search", "query", "--after=1"],
        vec!["board", "search", "query", "-n51"],
        vec!["board", "search", "query", "--plan=E1"],
        vec!["board", "search"],
    ] {
        let options = parse(&words.iter().map(|word| (*word).into()).collect::<Vec<_>>()).unwrap();
        assert!(board_parse(&options, None).is_err(), "{words:?}");
    }
}
