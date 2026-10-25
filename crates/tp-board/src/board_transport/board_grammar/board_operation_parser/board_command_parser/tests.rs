use crate::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
use crate::board_protocol::BoardOp;

#[test]
fn transfer_and_lifecycle_notes_survive_cli_forwarding() {
    for words in [
        vec!["board", "transfer", "P7.3", "P8", "Preserve", "the trail"],
        vec!["board", "close", "P7", "Implementation complete"],
        vec!["board", "reopen", "P7", "Follow-up work"],
    ] {
        let args = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        let options = crate::board_test_support::parse_board_arguments(&args).unwrap();
        let original = board_parse(&options, None).unwrap();
        let forwarded = normalize_args(&options, None).unwrap();
        assert_eq!(
            board_parse(&crate::board_test_support::round_trip(&forwarded), None).unwrap(),
            original
        );
        match original {
            BoardCommand::Op(BoardOp::TaskTransfer {
                task,
                destination,
                note,
            }) => {
                assert_eq!(task.to_string(), "P7.3");
                assert_eq!(destination.to_string(), "P8");
                assert_eq!(note.as_str(), "Preserve the trail");
            }
            BoardCommand::Op(BoardOp::PlanClose { plan, note }) => {
                assert_eq!(plan.to_string(), "P7");
                assert_eq!(note.as_str(), "Implementation complete");
            }
            BoardCommand::Op(BoardOp::PlanReopen { plan, note }) => {
                assert_eq!(plan.to_string(), "P7");
                assert_eq!(note.as_str(), "Follow-up work");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}

#[test]
fn transfer_and_lifecycle_require_notes_and_reject_unrelated_options() {
    for words in [
        vec!["board", "transfer", "P7.3", "P8"],
        vec!["board", "transfer", "P7.3", "P7", "Same plan"],
        vec!["board", "transfer", "P7", "P8", "Wrong source"],
        vec!["board", "transfer", "P7.3", "P8.1", "Wrong destination"],
        vec!["board", "close", "P7"],
        vec!["board", "reopen", "P7"],
        vec!["board", "close", "P7.3", "Wrong target"],
        vec!["board", "close", "P7", "Note", "--to=codex"],
        vec!["board", "close", "P7", "Note", "--no-cache"],
    ] {
        let args = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        let options = crate::board_test_support::parse_board_arguments(&args).unwrap();
        assert!(board_parse(&options, None).is_err(), "{words:?}");
    }
}
