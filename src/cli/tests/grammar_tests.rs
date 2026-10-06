use super::super::*;

#[test]
fn board_defaults_allocate_review_space_and_preserve_explicit_budgets() {
    for (words, budget) in [
        (&["board"][..], 1_500),
        (&["board", "inbox"][..], 1_500),
        (&["board", "show", "P7"][..], 4_000),
        (&["board", "review", "P7@12"][..], 4_000),
        (&["feedback", "blocked", "missing index"][..], 1_500),
    ] {
        let words = words.iter().map(|word| (*word).into()).collect::<Vec<_>>();
        let options = parse(&words).unwrap();
        assert_eq!(options.budget, budget);
        validate(&options).unwrap();
        let mut explicit = vec!["-b700".into()];
        explicit.extend(words);
        assert_eq!(parse(&explicit).unwrap().budget, 700);
    }
}

#[test]
fn board_flags_are_rejected_on_source_search_verbs() {
    for flag in [
        "--body=plan.md",
        "--to=codex",
        "--supersedes=E480",
        "--steward=claude",
        "--plan=P7",
        "--scope=parser",
        "--section=Grammar",
        "--open",
        "--board-text=- item",
        "--agent-model=gpt-6",
        "--agent-effort=xhigh",
        "--recent-calls=[]",
        "--after=P7",
        "--through=90",
    ] {
        let options = parse(&["search".into(), "query".into(), flag.into()]).unwrap();
        let error = validate(&options).unwrap_err().to_string();
        assert!(error.starts_with("invalid_options:"), "{flag}: {error}");
    }
}

#[test]
fn board_grammar_accepts_every_m1_command_and_skill_example() {
    use crate::board::board_grammar::{BoardCommand, parse as board_parse};
    use crate::board::board_protocol::BoardOp;
    let examples: &[(&[&str], Option<&str>, &str)] = &[
        (&["board", "hello", "gpt-6.1-sol", "xhigh"], None, "hello"),
        (&["board", "hello", "exact-model-id"], None, "hello"),
        (&["board"], None, "inbox"),
        (&["board", "inbox"], None, "inbox"),
        (&["board", "inbox", "--wait"], None, "inbox"),
        (&["board", "inbox", "5120", "--wait"], None, "inbox"),
        (&["board", "5120"], None, "inbox"),
        (&["board", "show"], None, "overview"),
        (&["board", "show", "P7"], None, "show"),
        (&["board", "show", "P7@12"], None, "show"),
        (&["board", "show", "P7@10.."], None, "show"),
        (&["board", "show", "P7@12.."], None, "show"),
        (&["board", "show", "P7@10..14"], None, "show"),
        (
            &[
                "board",
                "claim",
                "P7.3",
                "parser + tests; excludes review packet",
            ],
            None,
            "claim_task",
        ),
        (
            &[
                "board",
                "claim",
                "P7",
                "Parser",
                "--scope",
                "grammar + tests",
                "--section",
                "CLI",
            ],
            None,
            "carve_claim",
        ),
        (
            &[
                "board",
                "post",
                "P7.3",
                "progress",
                "Parser accepts P7@12; tests pass",
            ],
            None,
            "post",
        ),
        (
            &[
                "board",
                "post",
                "P7",
                "question",
                "should ingest include tags?",
                "--to",
                "josh",
            ],
            None,
            "post",
        ),
        (
            &[
                "board",
                "post",
                "P7",
                "answer",
                "E482: yes",
                "--supersedes",
                "E482",
            ],
            None,
            "post",
        ),
        (
            &[
                "board",
                "task",
                "P7",
                "Review packet trimming",
                "--to",
                "codex",
            ],
            None,
            "task_create",
        ),
        (&["board", "task", "P7.3", "todo"], None, "task_move"),
        (
            &["board", "task", "P7.3", "doing", "--to", "claude"],
            None,
            "task_move",
        ),
        (&["board", "task", "P7.3", "review"], None, "task_move"),
        (&["board", "task", "P7.3", "done"], None, "task_move"),
        (&["board", "task", "P7.3", "blocked"], None, "task_move"),
        (
            &["board", "post", "P7", "question", "...", "--to", "codex"],
            None,
            "post",
        ),
        (
            &["board", "post", "P7", "review", "review evidence"],
            None,
            "post",
        ),
        (
            &["board", "post", "P7", "divergence", "crossed lane"],
            None,
            "post",
        ),
        (
            &["board", "post", "P7", "decision", "accepted scope"],
            None,
            "post",
        ),
        (
            &[
                "board",
                "propose",
                "P7@12",
                "--body",
                "plan.md",
                "Clarify parser scope",
            ],
            Some("# Updated plan\n- parser"),
            "propose",
        ),
        (&["board", "review", "P7@12", "codex"], None, "review"),
        (&["board", "accept", "E485"], None, "accept"),
        (
            &["board", "accept", "E485", "accepted after review"],
            None,
            "accept",
        ),
        (
            &["board", "reject", "E485", "rebase the scope"],
            None,
            "reject",
        ),
        (
            &["board", "edit", "P7@12", "--body", "-", "direct correction"],
            Some("# Revised plan"),
            "edit",
        ),
        (
            &[
                "board",
                "new",
                "Parser plan",
                "--steward",
                "claude",
                "--body",
                "plan.md",
            ],
            Some("# Grammar"),
            "new",
        ),
        (&["board", "new", "Empty plan"], None, "new"),
        (&["board", "ingest"], None, "ingest"),
        (
            &[
                "board",
                "link",
                "0123456789abcdef0123456789abcdef01234567",
                "P7.3",
            ],
            None,
            "link_commit",
        ),
        (
            &[
                "feedback",
                "blocked",
                "Router unavailable",
                "--body",
                "feedback.md",
            ],
            Some("what I tried\nwhat happened\nworkaround\nwhat would help"),
            "feedback",
        ),
        (
            &[
                "feedback",
                "blocked",
                "router failed",
                "--body",
                "report.md",
                "--plan",
                "P7",
            ],
            Some("what I tried\nwhat happened\nworkaround\nwhat would help"),
            "feedback",
        ),
        (
            &["feedback", "confused", "could not choose a scope"],
            None,
            "feedback",
        ),
        (
            &["feedback", "wrong", "missing current bytes"],
            None,
            "feedback",
        ),
        (
            &["feedback", "missing", "tags are unavailable"],
            None,
            "feedback",
        ),
        (&["feedback", "ls"], None, "feedback_list"),
        (&["feedback", "ls", "--open"], None, "feedback_list"),
        (
            &[
                "feedback",
                "close",
                "E512",
                "fixed",
                "E513 and full commit oid",
            ],
            None,
            "feedback_close",
        ),
        (
            &["feedback", "close", "E512", "wontfix"],
            None,
            "feedback_close",
        ),
        (
            &["feedback", "close", "E512", "duplicate", "E500"],
            None,
            "feedback_close",
        ),
    ];
    for (words, body, expected) in examples {
        let words = words.iter().map(|word| (*word).into()).collect::<Vec<_>>();
        let options = parse(&words).unwrap();
        let command =
            board_parse(&options, *body).unwrap_or_else(|error| panic!("{words:?}: {error}"));
        let actual = match command {
            BoardCommand::Ingest => "ingest".into(),
            BoardCommand::Web { .. } => "web".into(),
            BoardCommand::Op(BoardOp::Feedback { import_key, .. }) => {
                uuid::Uuid::parse_str(&import_key.unwrap().to_string()).unwrap();
                "feedback".into()
            }
            BoardCommand::Op(op) => serde_json::to_value(op).unwrap()["op"]
                .as_str()
                .unwrap()
                .to_owned(),
        };
        assert_eq!(actual, *expected, "{words:?}");
    }
}

#[test]
fn board_grammar_rejects_cross_verb_flags_and_invalid_domain_references() {
    use crate::board::board_grammar::parse as board_parse;
    for words in [
        &["board", "show", "P7", "--to", "codex"][..],
        &["board", "claim", "P7.3", "scope", "--scope", "other scope"][..],
        &["board", "claim", "P7", "title"][..],
        &["board", "task", "P7.3", "doing", "--section", "Grammar"][..],
        &["board", "post", "P7@12", "note", "text"][..],
        &["board", "post", "P7", "claim", "text"][..],
        &["board", "review", "P7"][..],
        &["board", "propose", "P7@12", "summary"][..],
        &["board", "show", "--wait"][..],
        &["board", "hello", "model", "effort", "extra"][..],
        &["board", "link", "not-an-oid", "P7.3"][..],
        &[
            "board",
            "link",
            "0123456789abcdef0123456789abcdef01234567",
            "P7",
        ][..],
        &[
            "board",
            "link",
            "0123456789abcdef0123456789abcdef01234567",
            "P7.3",
            "extra",
        ][..],
        &[
            "board",
            "link",
            "0123456789abcdef0123456789abcdef01234567",
            "P7.3",
            "--to",
            "codex",
        ][..],
        &["board", "inbox", "18446744073709551615"][..],
        &["board", "inbox", "+1"][..],
        &["board", "inbox", "00"][..],
        &["feedback", "close", "E512", "open"][..],
        &["feedback", "close", "E512", "triaged"][..],
        &["feedback", "ls", "--plan", "P7"][..],
        &["feedback", "wrong", "summary", "--open"][..],
    ] {
        let options = parse(&words.iter().map(|word| (*word).into()).collect::<Vec<_>>()).unwrap();
        assert!(board_parse(&options, None).is_err(), "accepted {words:?}");
    }
    let task = parse(&["board".into(), "show".into(), "P7.3".into()]).unwrap();
    assert!(board_parse(&task, None).is_ok());
}
