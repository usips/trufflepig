use crate::cli::parse;

#[test]
fn board_collection_grammar_uses_frozen_typed_cursors_and_required_show_targets() {
    use crate::board::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
    use crate::board::board_protocol::{BoardOp, EntryCursor};
    let examples = [
        vec!["board", "show"],
        vec!["board", "show", "--after=P7", "--through=90", "-n3"],
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
    let BoardCommand::Op(BoardOp::Overview { limit, .. }) =
        board_parse(&parse(&["board".into(), "show".into()]).unwrap(), None).unwrap()
    else {
        panic!("overview");
    };
    assert_eq!(limit, 200);
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
