use super::super::*;

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
    let args = ["board", "claim", "P7.3", "--resume=E42"].map(str::to_owned);
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
    let args = ["board", "claim", "P7.3", "--resume=bogus"].map(str::to_owned);
    let options = parse(&args).unwrap();
    assert!(board_parse(&options, None).is_err());
}

#[test]
fn claim_resume_value_requires_equals_and_leaves_positionals_alone() {
    use crate::board::board_grammar::{BoardCommand, normalize_args, parse as board_parse};
    use crate::board::board_protocol::{BoardOp, ClaimResume};
    // Scope words after a bare `--resume` stay scope; nothing is eaten.
    let args = ["board", "claim", "P7.3", "--resume", "fresh", "scope"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let BoardCommand::Op(BoardOp::ClaimTask {
        task,
        scope,
        resume,
        ..
    }) = board_parse(&options, None).unwrap()
    else {
        panic!("expected claim op")
    };
    assert_eq!(task.to_string(), "P7.3");
    assert_eq!(resume, ClaimResume::Idle);
    assert_eq!(scope.unwrap().as_str(), "fresh scope");
    // Flag-before-target: the task ref is not eaten as the resume value.
    let args = ["board", "claim", "--resume", "P7.3"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let BoardCommand::Op(BoardOp::ClaimTask { task, resume, .. }) =
        board_parse(&options, None).unwrap()
    else {
        panic!("expected claim op")
    };
    assert_eq!(task.to_string(), "P7.3");
    assert_eq!(resume, ClaimResume::Idle);
    // The `=` form still names an explicit entry takeover.
    let args = ["board", "claim", "P7.3", "--resume=E485"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let command = board_parse(&options, None).unwrap();
    let forwarded = normalize_args(&args, &options, None).unwrap();
    let routed = parse(&forwarded).unwrap();
    assert_eq!(board_parse(&routed, None).unwrap(), command);
    let BoardCommand::Op(BoardOp::ClaimTask { resume, .. }) = command else {
        panic!("expected claim op")
    };
    assert_eq!(
        resume,
        ClaimResume::Entry(crate::board::board_ids::EntryId::new(485).unwrap())
    );
    // Space form with a bare entry token is rejected: it meant --resume=E42.
    let args = ["board", "claim", "P7.3", "--resume", "E42"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let error = board_parse(&options, None).unwrap_err().to_string();
    assert!(error.contains("use --resume=E42"), "{error}");
}

#[test]
fn claim_resume_space_form_entry_token_points_at_equals_form() {
    use crate::board::board_grammar::{BoardCommand, parse as board_parse};
    use crate::board::board_protocol::{BoardOp, ClaimResume};
    let args = ["board", "claim", "P7.3", "--resume", "E5"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let error = board_parse(&options, None).unwrap_err().to_string();
    assert!(error.contains("use --resume=E5"), "{error}");
    let args = ["board", "claim", "P7.3", "--resume=E5"].map(str::to_owned);
    let options = parse(&args).unwrap();
    let BoardCommand::Op(BoardOp::ClaimTask { scope, resume, .. }) =
        board_parse(&options, None).unwrap()
    else {
        panic!("expected claim op")
    };
    assert_eq!(
        resume,
        ClaimResume::Entry(crate::board::board_ids::EntryId::new(5).unwrap())
    );
    assert!(scope.is_none());
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
