use crate::board::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
use crate::board::board_protocol::BoardOp;
use crate::cli::parse;

#[test]
fn project_selectors_survive_router_normalization_for_each_read() {
    for command in [None, Some("show"), Some("inbox"), Some("attention")] {
        for selector in ["W1", "unscoped", "-Dash", &"a".repeat(64)] {
            let mut words = vec!["board"];
            words.extend(command);
            words.extend(["--project", selector]);
            let args: Vec<_> = words.into_iter().map(str::to_owned).collect();
            let options = parse(&args).unwrap();
            let original = board_parse(&options, None).unwrap();
            let forwarded = normalize_args(&args, &options, None).unwrap();
            let forwarded = parse(&forwarded).unwrap();
            assert_eq!(forwarded.board.project.as_deref(), Some(selector));
            assert_eq!(board_parse(&forwarded, None).unwrap(), original);
        }
    }
}

#[test]
fn project_options_reject_all_empty_values_and_inapplicable_commands() {
    for words in [
        vec!["board", "show", "--all", "--project=W1"],
        vec!["board", "inbox", "--project=W1", "--all"],
        vec!["board", "attention", "--all", "--project=W1"],
        vec!["board", "show", "P1", "--project=W1"],
        vec!["board", "projects", "--project=W1"],
        vec!["board", "projects", "--all"],
        vec!["board", "feed", "--project=W1"],
        vec!["board", "hello", "model", "--project=W1"],
        vec!["board", "new", "Title", "--project=W1"],
        vec!["board", "show", "--project="],
        vec!["board", "show", "--project", " "],
        vec!["feedback", "ls", "--project=W1"],
        vec!["search", "needle", "--project=W1"],
    ] {
        let args: Vec<_> = words.iter().map(|word| (*word).to_owned()).collect();
        let options = parse(&args).unwrap();
        let error = board_parse(&options, None).unwrap_err();
        assert!(
            error.to_string().starts_with("invalid_options:"),
            "{words:?}: {error}"
        );
    }
}

#[test]
fn projects_grammar_accepts_a_registry_read_without_a_target() {
    let options = parse(&["board".into(), "projects".into()]).unwrap();
    assert!(matches!(
        board_parse(&options, None).unwrap(),
        BoardCommand::Op(BoardOp::Projects)
    ));
    let options = parse(&["board".into(), "projects".into(), "W1".into()]).unwrap();
    assert!(
        board_parse(&options, None)
            .unwrap_err()
            .to_string()
            .starts_with("usage: board projects")
    );
}
