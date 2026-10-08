use crate::board::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
use crate::board::board_protocol::{BoardOp, TaskOrder};
use crate::board::board_vocabulary::TaskColumn;
use crate::cli::parse;

#[test]
fn done_cli_round_trips_plan_project_and_recent_cursor() {
    for words in [
        vec!["board", "done"],
        vec!["board", "done", "--all", "-n2"],
        vec![
            "board",
            "done",
            "--project",
            "Space's Fleet",
            "--after=9:P7.3",
        ],
        vec![
            "board",
            "done",
            "P7",
            "--project=W1",
            "--after=9:P7.3",
            "-n2",
        ],
    ] {
        let args = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        let options = parse(&args).unwrap();
        let original = board_parse(&options, None).unwrap();
        let forwarded = normalize_args(&args, &options, None).unwrap();
        assert_eq!(
            board_parse(&parse(&forwarded).unwrap(), None).unwrap(),
            original
        );
        if let Some(project) = options.board.project.as_deref() {
            assert!(forwarded.contains(&format!("--project={project}")));
        }
    }
    let args = ["board", "done", "P7", "--after=9:P7.3", "-n2"].map(str::to_owned);
    let BoardCommand::Op(BoardOp::Tasks {
        plan,
        column,
        order,
        before,
        limit,
        ..
    }) = board_parse(&parse(&args).unwrap(), None).unwrap()
    else {
        panic!("plan Done");
    };
    assert_eq!(plan.to_string(), "P7");
    assert_eq!(column, Some(TaskColumn::Done));
    assert_eq!(order, TaskOrder::RecentFirst);
    assert_eq!(before.unwrap().id.to_string(), "P7.3");
    assert_eq!(limit, 2);
    let BoardCommand::Op(BoardOp::DoneTasks { limit, .. }) =
        board_parse(&parse(&["board".into(), "done".into()]).unwrap(), None).unwrap()
    else {
        panic!("board Done");
    };
    assert_eq!(limit, 200);
}

#[test]
fn done_cli_rejects_bad_recent_cursors_and_scopes() {
    for words in [
        vec!["board", "done", "--after=P7.3"],
        vec!["board", "done", "--after=0:P7.3"],
        vec!["board", "done", "--after=9:E7"],
        vec!["board", "done", "P7", "--after=9:P8.3"],
        vec!["board", "done", "P7.3"],
        vec!["board", "done", "P7", "--all"],
        vec!["board", "done", "--project=W1", "--all"],
        vec!["board", "done", "--through=9"],
        vec!["board", "done", "-n201"],
    ] {
        let args = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        assert!(
            parse(&args)
                .and_then(|options| board_parse(&options, None))
                .is_err(),
            "{words:?}"
        );
    }
}
