use super::*;
use crate::search;

#[test]
fn reconciliation_resumes_preparation_and_new_generations() -> anyhow::Result<()> {
    use crate::semantic::{DIMENSIONS, Embedding, preparation};
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    std::fs::write(root.path().join("source.rs"), "fn example() {}")?;
    let mut store = Store::open(root.path(), cache.path())?;
    store.index()?;
    let receipt = preparation::schedule(root.path(), cache.path())?;
    let database = rusqlite::Connection::open(cache.path().join("preparation.sqlite3"))?;
    database.execute("INSERT INTO preparation_runs(generation,state,cursor,total,cached,missing,failures,error,updated_ms) VALUES(?1,'running',0,1,0,1,0,NULL,0)", [receipt.captured_generation])?;
    let manager = preparation::PreparationManager::new(preparation::ClosureWorker(
        |inputs: &[preparation::EmbeddingInput]| {
            Ok(inputs
                .iter()
                .map(|_| {
                    let mut vector = [0.0; DIMENSIONS];
                    vector[0] = 1.0;
                    Ok(Embedding(vector))
                })
                .collect())
        },
    ));
    root_daemon::schedule_pending_preparation(&manager, root.path(), cache.path())?;
    let done = preparation::wait_timeout(
        root.path(),
        cache.path(),
        receipt.captured_generation,
        Duration::from_secs(3),
    )?;
    assert_eq!(done.state, preparation::PreparationState::Completed);
    std::fs::write(root.path().join("source.rs"), "fn changed() {}")?;
    store.index()?;
    root_daemon::schedule_pending_preparation(&manager, root.path(), cache.path())?;
    let done = preparation::wait_timeout(
        root.path(),
        cache.path(),
        store.generation()?,
        Duration::from_secs(3),
    )?;
    assert_eq!(done.state, preparation::PreparationState::Completed);
    Ok(())
}

#[test]
fn daemon_dispatch_rejects_wrong_root_and_unbounded_options() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut options = parse(&[
        "--root".into(),
        other.path().display().to_string(),
        "status".into(),
    ])
    .unwrap();
    assert!(
        local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string()
            .contains("invalid_root")
    );
    options.root = root.path().to_owned();
    options.limit = 0;
    assert!(
        local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string()
            .contains("invalid_limit")
    );
    options.limit = 1;
    options.budget = usize::MAX;
    assert!(
        local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string()
            .contains("invalid_budget")
    );
}

#[test]
fn system_routes_gates_internal_and_local_verbs() {
    let routed = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let options = parse(&args).unwrap();
        let verb = options
            .words
            .first()
            .map(String::as_str)
            .unwrap_or("status");
        system_routes(&options, verb)
    };
    for args in [
        &["serve"][..],
        &["workspace-serve"][..],
        &["system-serve"][..],
        &["system"][..],
        &["ws"][..],
        &["stop"][..],
        &["index"][..],
        &["init"][..],
        &["semantic-check", "model"][..],
        &["semantic", "status"][..],
        &["--no-daemon", "search", "query"][..],
    ] {
        assert!(!routed(args), "expected no system route for {args:?}");
    }
    for args in [
        &["search", "query"][..],
        &["show", "src/lib.rs"][..],
        &["ctx", "HANDLE"][..],
        &["refs", "name"][..],
        &["map"][..],
        &["hist", "src/lib.rs"][..],
        &["blame", "src/lib.rs"][..],
        &["diff", "src/lib.rs"][..],
        &["audit"][..],
        &["doctor"][..],
        &["semantic", "prepare"][..],
        &["hist-index"][..],
    ] {
        assert!(routed(args), "expected a system route for {args:?}");
    }
}

#[test]
fn unknown_commands_are_rejected_before_root_validation() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    for command in ["statuss", "not-a-command"] {
        let options = parse(&[
            "--root".into(),
            other.path().display().to_string(),
            command.into(),
        ])
        .unwrap();
        let error = local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown_command"), "{error}");
        assert!(error.contains("search"), "{error}");
        assert!(!error.contains("invalid_root"), "{error}");
    }
}

#[test]
fn explicit_search_accepts_command_words() {
    let options = parse(&["search".into(), "status".into(), "not-a-command".into()]).unwrap();
    validate(&options).unwrap();
    assert_eq!(
        options.words,
        vec![
            "search".to_owned(),
            "status".to_owned(),
            "not-a-command".to_owned(),
        ]
    );
}

#[test]
fn negative_path_filter_is_quoted_inside_the_query_argument() {
    let options = parse(&["search".into(), "needle -file:tests".into()]).unwrap();
    let query = search::Query::parse(&options.words[1..].join(" ")).unwrap();
    assert_eq!(query.text, "needle");
    assert_eq!(query.path.excludes(), ["tests"]);

    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    let query = search::Query::parse(&forwarded.words[1..].join(" ")).unwrap();
    assert_eq!(query.path.excludes(), ["tests"]);

    assert!(parse(&["search".into(), "needle".into(), "-file:tests".into()]).is_err());
    assert!(parse(&["search".into(), "needle".into(), "--bogus".into()]).is_err());
}

#[test]
fn client_normalization_preserves_regex_spaces() {
    let options = parse(&[
        "search".into(),
        "re:a  b".into(),
        "--budget".into(),
        "500".into(),
    ])
    .unwrap();
    let root = Path::new("/example");
    let forwarded = parse(&normalized_args(&options, root)).unwrap();
    assert_eq!(forwarded.root, root);
    assert_eq!(forwarded.budget, 500);
    assert_eq!(
        search::Query::parse(&forwarded.words[1..].join(" "))
            .unwrap()
            .text,
        "a  b"
    );
}

#[test]
fn client_normalization_forwards_semantic_overrides() {
    let options = parse(&["--sem".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(forwarded.sem);
    assert!(!forwarded.no_sem);

    let options = parse(&["--no-sem".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(!forwarded.sem);
    assert!(forwarded.no_sem);
}

#[test]
fn semantic_flags_are_mutually_exclusive() {
    let error = parse(&["--sem".into(), "--no-sem".into(), "search".into()]).unwrap_err();
    assert!(error.to_string().contains("cannot be used with"));
}

#[test]
fn explicit_budget_is_recorded_only_when_passed() {
    let implicit = parse(&["search".into(), "query".into()]).unwrap();
    assert!(!implicit.explicit_budget);
    assert_eq!(implicit.budget, 600);
    let long = parse(&[
        "--budget".into(),
        "900".into(),
        "search".into(),
        "query".into(),
    ])
    .unwrap();
    assert!(long.explicit_budget);
    let short = parse(&["-b".into(), "900".into(), "search".into(), "query".into()]).unwrap();
    assert!(short.explicit_budget);
    let joined = parse(&["-b900".into(), "search".into(), "query".into()]).unwrap();
    assert!(joined.explicit_budget);
    assert_eq!(joined.budget, 900);
    // Source reads default to a larger budget unless one is given.
    let show = parse(&["show".into(), "path:a.rs".into()]).unwrap();
    assert_eq!(show.budget, super::SHOW_BUDGET);
    let small = parse(&["-b".into(), "300".into(), "show".into(), "x".into()]).unwrap();
    assert_eq!(small.budget, 300);
}

#[test]
fn client_normalization_forwards_rerank_overrides() {
    let options = parse(&["--rerank".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(forwarded.rerank);
    assert!(!forwarded.no_rerank);

    let options = parse(&["--no-rerank".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(!forwarded.rerank);
    assert!(forwarded.no_rerank);
}

#[test]
fn rerank_flags_are_mutually_exclusive() {
    let error = parse(&["--rerank".into(), "--no-rerank".into(), "search".into()]).unwrap_err();
    assert!(error.to_string().contains("cannot be used with"));
}

#[test]
fn client_normalization_forwards_format() {
    let options = parse(&[
        "--format".into(),
        "lines".into(),
        "search".into(),
        "q".into(),
    ])
    .unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert_eq!(forwarded.format, "lines");
    assert_eq!(
        forwarded.output_format(),
        crate::output::OutputFormat::Lines
    );
    let implicit = parse(&["search".into(), "q".into()]).unwrap();
    assert_eq!(implicit.output_format(), crate::output::OutputFormat::Json);
}

#[test]
fn json_flag_conflicts_with_lines_format() {
    let options = parse(&[
        "--json".into(),
        "--format".into(),
        "lines".into(),
        "status".into(),
    ])
    .unwrap();
    assert!(
        validate(&options)
            .unwrap_err()
            .to_string()
            .contains("--json conflicts with --format lines")
    );
    assert!(parse(&["--format".into(), "yaml".into(), "status".into()]).is_err());
}

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

#[test]
fn board_transport_preserves_markdown_body_text_flags_and_feedback_key() {
    use crate::board::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
    use crate::board::board_protocol::BoardOp;
    let args = [
        "board",
        "propose",
        "P7@12",
        "--body",
        "plan.md",
        "--",
        "- repair parser",
    ]
    .map(str::to_owned);
    let options = parse(&args).unwrap();
    let body = "- one\n- two\n\"quoted\" \\ body";
    let original = board_parse(&options, Some(body)).unwrap();
    let forwarded = normalize_args(&args, &options, Some(body)).unwrap();
    assert!(
        !forwarded
            .iter()
            .any(|arg| arg == "--" || arg.starts_with("--body"))
    );
    assert!(
        forwarded
            .iter()
            .any(|arg| arg.starts_with("--board-payload="))
    );
    let routed = parse(&forwarded).unwrap();
    assert!(routed.root.is_absolute());
    assert_eq!(board_parse(&routed, None).unwrap(), original);

    let args = [
        "feedback",
        "wrong",
        "- missing bytes",
        "--plan=P7",
        "--agent-model=gpt-6",
        "--agent-effort=xhigh",
    ]
    .map(str::to_owned);
    // Hidden text supports a leading hyphen without a positional argument.
    let mut args = args.to_vec();
    args[2] = "--board-text=- missing bytes".into();
    let options = parse(&args).unwrap();
    let forwarded = normalize_args(&args, &options, None).unwrap();
    let routed = parse(&forwarded).unwrap();
    assert_eq!(routed.board.agent_model.as_deref(), Some("gpt-6"));
    assert_eq!(routed.board.agent_effort.as_deref(), Some("xhigh"));
    let first = board_parse(&routed, None).unwrap();
    let second = board_parse(&routed, None).unwrap();
    assert_eq!(first, second);
    assert!(matches!(
        first,
        BoardCommand::Op(BoardOp::Feedback {
            import_key: Some(_),
            ..
        })
    ));
}

#[test]
fn board_transport_rejects_ambiguous_text_and_oversized_feedback_audit() {
    use crate::board::board_grammar::parse as board_parse;
    let options = parse(&[
        "board".into(),
        "post".into(),
        "P7".into(),
        "note".into(),
        "text".into(),
        "--board-text=other".into(),
    ])
    .unwrap();
    assert!(
        board_parse(&options, None)
            .unwrap_err()
            .to_string()
            .starts_with("invalid_options:")
    );
    let options = parse(&[
        "feedback".into(),
        "wrong".into(),
        "summary".into(),
        format!("--recent-calls={}", "x".repeat(2_049)),
    ])
    .unwrap();
    assert!(
        validate(&options)
            .unwrap_err()
            .to_string()
            .contains("2048 bytes")
    );
    let options = parse(&[
        "feedback".into(),
        "wrong".into(),
        "summary".into(),
        "--recent-calls=not-json".into(),
    ])
    .unwrap();
    assert!(
        board_parse(&options, None)
            .unwrap_err()
            .to_string()
            .starts_with("invalid_options:")
    );
}

#[test]
fn claim_resume_without_scope_survives_router_normalization() {
    use crate::board::board_grammar::{normalize_args, parse as board_parse};
    let args = ["board", "claim", "P7.3", "--resume"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let command = board_parse(&options, None).unwrap();
    let forwarded = normalize_args(&args, &options, None).unwrap();
    let routed = parse(&forwarded).unwrap();
    assert_eq!(board_parse(&routed, None).unwrap(), command);
    for words in [
        &[
            "board", "claim", "P7", "title", "--scope", "scope", "--resume",
        ][..],
        &["board", "task", "P7.3", "doing", "--resume"][..],
        &["board", "hello", "model", "--resume"][..],
    ] {
        let options = parse(
            &words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(board_parse(&options, None).is_err());
    }
}

#[test]
fn claim_resume_accepts_an_optional_entry_target() {
    use crate::board::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
    use crate::board::board_protocol::{BoardOp, ClaimResume};
    for words in [
        &["board", "claim", "P7.3", "--resume"][..],
        &["board", "claim", "P7.3", "--resume", "E42"][..],
        &["board", "claim", "P7.3", "--resume=E42"][..],
    ] {
        let args = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        let options = parse(&args).unwrap();
        let command = board_parse(&options, None).unwrap();
        let forwarded = normalize_args(&args, &options, None).unwrap();
        let routed = parse(&forwarded).unwrap();
        assert_eq!(board_parse(&routed, None).unwrap(), command);
    }
    let args = ["board", "claim", "P7.3", "--resume", "E42"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let BoardCommand::Op(op) = board_parse(&options, None).unwrap() else {
        panic!("expected op")
    };
    let BoardOp::ClaimTask {
        scope,
        resume: ClaimResume::Entry(entry),
        ..
    } = op
    else {
        panic!("expected an explicit entry resume: {op:?}")
    };
    assert_eq!(entry, crate::board::board_ids::EntryId::new(42).unwrap());
    assert!(scope.is_none());
    let args = ["board", "claim", "P7.3", "fresh", "scope", "--resume"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let BoardCommand::Op(op) = board_parse(&options, None).unwrap() else {
        panic!("expected op")
    };
    let BoardOp::ClaimTask { scope, resume, .. } = op else {
        panic!("expected claim op")
    };
    assert_eq!(resume, ClaimResume::Idle);
    assert_eq!(scope.unwrap().as_str(), "fresh scope");
    let args = ["board", "claim", "P7.3", "--resume", "bogus"].map(str::to_owned);
    let options = parse(&args).unwrap();
    assert!(board_parse(&options, None).is_err());
}

#[test]
fn claim_for_delegates_to_a_session_under_the_caller() {
    use crate::board::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
    use crate::board::board_protocol::BoardOp;
    for words in [
        &["board", "claim", "P7.3", "scope", "--for", "codex/c7"][..],
        &["board", "claim", "P7.3", "scope", "--for=codex/c7"][..],
    ] {
        let args = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        let options = parse(&args).unwrap();
        let command = board_parse(&options, None).unwrap();
        let forwarded = normalize_args(&args, &options, None).unwrap();
        let routed = parse(&forwarded).unwrap();
        assert_eq!(board_parse(&routed, None).unwrap(), command);
    }
    let args = ["board", "claim", "P7.3", "scope", "--for", "codex/c7"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let BoardCommand::Op(op) = board_parse(&options, None).unwrap() else {
        panic!("expected op")
    };
    let BoardOp::ClaimTask {
        delegate: Some(delegate),
        ..
    } = op
    else {
        panic!("expected a delegated claim: {op:?}")
    };
    assert_eq!(delegate.harness.as_str(), "codex");
    assert_eq!(delegate.session, "c7");
    for bad in [
        "",
        "/",
        "codex/",
        "/c7",
        "codex/c7/extra",
        "codex/c 7",
        " codex/c7",
    ] {
        let args = [
            "board".to_owned(),
            "claim".to_owned(),
            "P7.3".to_owned(),
            "scope".to_owned(),
            format!("--for={bad}"),
        ];
        let options = parse(&args).unwrap();
        let error = board_parse(&options, None).unwrap_err().to_string();
        assert!(error.starts_with("invalid_options:"), "{bad:?}: {error}");
    }
    let args = ["board", "task", "P7.3", "doing", "--for=codex/c7"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let error = board_parse(&options, None).unwrap_err().to_string();
    assert!(error.starts_with("invalid_options:"), "{error}");
    let args = ["board", "claim", "P7", "title", "--scope=s", "--for=c/c"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let error = board_parse(&options, None).unwrap_err().to_string();
    assert!(error.starts_with("invalid_options:"), "{error}");
}

#[test]
fn board_inbox_all_scope_survives_router_normalization() {
    use crate::board::board_grammar::{normalize_args, parse as board_parse};
    let args = ["board", "inbox", "42", "--all"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let command = board_parse(&options, None).unwrap();
    let forwarded = normalize_args(&args, &options, None).unwrap();
    let routed = parse(&forwarded).unwrap();
    assert_eq!(board_parse(&routed, None).unwrap(), command);
    assert!(matches!(
        command,
        crate::board::board_grammar::BoardCommand::Op(
            crate::board::board_protocol::BoardOp::Inbox {
                all: true,
                repo_key: None,
                ..
            }
        )
    ));
}

#[test]
fn board_text_is_always_raw_and_payload_is_separate() {
    use crate::board::board_grammar::{BoardCommand, parse as board_parse};
    use crate::board::board_protocol::BoardOp;
    let raw =
        r#"{"text":"decoded title","body":"injected body","import_key":null,"steer_mode":null}"#;
    let options = parse(&["board".into(), "new".into(), format!("--board-text={raw}")]).unwrap();
    let BoardCommand::Op(BoardOp::New { title, body, .. }) = board_parse(&options, None).unwrap()
    else {
        panic!("expected plan creation");
    };
    assert_eq!(title.as_str(), raw);
    assert_eq!(body.as_str(), "");
    let options = parse(&[
        "board".into(), "new".into(), "--board-text=raw".into(),
        "--board-payload={\"text\":\"payload\",\"body\":null,\"import_key\":null,\"steer_mode\":null}".into(),
    ]);
    assert!(options.is_err());
}

#[test]
fn board_edges_reject_search_flags_and_ignore_budget_text_after_separator() {
    use crate::board::board_grammar::parse as board_parse;
    for flag in [
        "--sem",
        "--no-sem",
        "--rerank",
        "--no-rerank",
        "--member=main",
        "--cache=cache",
    ] {
        let options = parse(&["board".into(), "show".into(), flag.into()]).unwrap();
        assert!(board_parse(&options, None).is_err(), "accepted {flag}");
    }
    let options = parse(&[
        "board".into(),
        "post".into(),
        "P7".into(),
        "note".into(),
        "--".into(),
        "-b".into(),
    ])
    .unwrap();
    assert!(!options.explicit_budget);
    assert_eq!(options.budget, 1500);
    let options = parse(&[
        "board".into(),
        "post".into(),
        "P7".into(),
        "note".into(),
        "--".into(),
        "-budget-like-text".into(),
    ])
    .unwrap();
    assert!(!options.explicit_budget);
}

#[test]
fn body_preflight_rejects_bad_grammar_before_client_reads() {
    use crate::board::board_grammar::{body_limit, validate_before_body};
    for words in [
        &["board", "show", "P1", "--body=-"][..],
        &["board", "propose", "not-a-revision", "summary", "--body=-"][..],
        &["feedback", "wrong", "summary", "--body=-", "--plan=bad"][..],
        &[
            "feedback",
            "wrong",
            "summary",
            "--body=-",
            "--recent-calls=bad",
        ][..],
    ] {
        let options = parse(&words.iter().map(|word| (*word).into()).collect::<Vec<_>>()).unwrap();
        assert!(
            validate_before_body(&options).is_err(),
            "accepted {words:?}"
        );
    }
    let feedback = parse(&[
        "feedback".into(),
        "wrong".into(),
        "summary".into(),
        "--body=-".into(),
    ])
    .unwrap();
    validate_before_body(&feedback).unwrap();
    assert_eq!(
        body_limit(&feedback),
        crate::board::board_vocabulary::ENTRY_TEXT_LIMIT
    );
    let plan = parse(&[
        "board".into(),
        "propose".into(),
        "P1@1".into(),
        "summary".into(),
        "--body=-".into(),
    ])
    .unwrap();
    validate_before_body(&plan).unwrap();
    assert_eq!(
        body_limit(&plan),
        crate::board::board_vocabulary::PLAN_TEXT_LIMIT
    );
}

#[test]
fn board_web_and_foreground_server_validate_without_workspace_dispatch() {
    use crate::board::board_grammar::{BoardCommand, parse as board_parse};
    for target in ["P7", "P7@12", "E512"] {
        let options = parse(&["board".into(), "web".into(), target.into()]).unwrap();
        assert!(matches!(
            board_parse(&options, None).unwrap(),
            BoardCommand::Web { target: Some(_) }
        ));
    }
    for target in [
        "P7.3",
        "P7@1..2",
        "0123456789abcdef0123456789abcdef01234567",
    ] {
        let options = parse(&["board".into(), "web".into(), target.into()]).unwrap();
        assert!(board_parse(&options, None).is_err(), "{target}");
    }
    for address in ["127.0.0.1:0", "[::1]:0"] {
        let options = parse(&["board-serve".into(), format!("--listen={address}")]).unwrap();
        validate(&options).unwrap();
        assert_eq!(options.listen.unwrap().port(), 0);
    }
    let defaults = parse(&["board-serve".into()]).unwrap();
    validate(&defaults).unwrap();
    assert_eq!(
        defaults.board_listen_address(),
        "127.0.0.1:7341".parse().unwrap()
    );
    for words in [
        vec!["board-serve", "--listen=0.0.0.0:7341"],
        vec!["board-serve", "--listen=[::]:7341"],
        vec!["board-serve", "--no-daemon"],
        vec!["board-serve", "--sem"],
        vec!["board-serve", "extra"],
        vec!["board", "show", "--listen=127.0.0.1:7341"],
        vec!["semantic-worker-serve", "--no-daemon"],
    ] {
        let options = parse(&words.iter().map(|word| (*word).into()).collect::<Vec<_>>()).unwrap();
        assert!(validate(&options).is_err(), "{words:?}");
    }
}
