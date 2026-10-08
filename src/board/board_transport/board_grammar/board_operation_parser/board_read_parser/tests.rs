use crate::cli::parse;
mod done_grammar_tests;

#[test]
fn board_collection_grammar_uses_frozen_typed_cursors_and_required_show_targets() {
    use crate::board::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
    use crate::board::board_protocol::{BoardOp, EntryCursor};
    let examples = [
        vec!["board", "show"],
        vec!["board", "show", "--after=P7", "--through=90", "-n3"],
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
        let forwarded = normalize_args(&args, &options, None).unwrap();
        assert_eq!(
            board_parse(&parse(&forwarded).unwrap(), None).unwrap(),
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
    use crate::board::board_grammar::parse as board_parse;
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
fn board_search_round_trip_preserves_raw_query_and_plan_scope() {
    use crate::board::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
    use crate::board::board_protocol::BoardOp;
    let args = [
        "board",
        "search",
        "--board-text=\"parser scope\" OR feedback",
        "--plan=P7",
    ]
    .map(str::to_owned);
    let options = parse(&args).unwrap();
    let forwarded = normalize_args(&args, &options, None).unwrap();
    let BoardCommand::Op(BoardOp::Search { query, plan, limit }) =
        board_parse(&parse(&forwarded).unwrap(), None).unwrap()
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
