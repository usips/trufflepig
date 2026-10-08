use super::*;

#[test]
fn llama_model_maps_to_muse() {
    let harness = HarnessLabel::parse("omp").unwrap();
    assert_eq!(
        serde_json::to_value(claim_vendor(&harness, Some("Llama 4 Maverick"))).unwrap(),
        "muse"
    );
}

#[test]
fn claim_vendor_preserves_model_priority_and_harness_fallback() {
    for (harness, model, expected) in [
        ("muse", Some(" Claude Sonnet 4.5 "), "claude"),
        ("omp", Some("gpt-6.1-sol"), "codex"),
        ("muse", Some("Codex"), "codex"),
        ("omp", Some("ChatGPT"), "codex"),
        ("muse", Some("Kimi K2"), "kimi"),
        ("muse", Some("Grok 4"), "grok"),
        ("omp", Some("Gemini 3 Pro"), "gemini"),
        ("omp", Some("Qwen3"), "qwen"),
        ("omp", Some("Muse Spark"), "muse"),
        ("codex", Some("unrecognized"), "codex"),
        ("claude", None, "claude"),
        ("kimi", None, "kimi"),
        ("grok", None, "grok"),
        ("gemini", None, "gemini"),
        ("qwen", None, "qwen"),
        ("muse", None, "muse"),
        ("cli", None, "human"),
        ("cli", Some("gpt-6.1-sol"), "human"),
        ("human", Some("Claude Sonnet 4.5"), "human"),
        ("omp", Some("unrecognized"), "unknown"),
        ("unrecognized", None, "unknown"),
    ] {
        let harness = HarnessLabel::parse(harness).unwrap();
        let vendor = claim_vendor(&harness, model);
        assert_eq!(vendor.as_str(), expected, "{harness} / {model:?}");
        assert_eq!(serde_json::to_value(vendor).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<AgentVendor>(serde_json::json!(expected)).unwrap(),
            vendor
        );
    }
    assert!(serde_json::from_value::<AgentVendor>(serde_json::json!("omp")).is_err());
}

#[test]
fn full_identity_round_trips_and_has_no_ambiguous_separators() {
    let actor = BoardActor::parse("josh@laptop/codex/c1").unwrap();
    assert_eq!(actor.identity(), "josh@laptop/codex/c1");
    assert_eq!(
        serde_json::from_str::<BoardActor>(&serde_json::to_string(&actor).unwrap()).unwrap(),
        actor
    );
    for invalid in [
        "josh@laptop/codex",
        "josh@laptop/codex/c1/extra",
        "@laptop/codex/c1",
        "josh@laptop/codex/ ",
    ] {
        assert!(BoardActor::parse(invalid).is_err());
    }
    assert!(HarnessLabel::parse("human").unwrap().is_human());
    assert!(HarnessLabel::parse("cli").unwrap().is_cli());
    assert!(HarnessLabel::parse("Codex").is_err());
    assert!(HarnessLabel::parse("git:person@example.com").is_ok());
}

#[test]
fn recipients_match_user_harness_or_complete_actor() {
    let actor = BoardActor::parse("josh@laptop/codex/c1").unwrap();
    assert!(BoardRecipient::parse("josh").unwrap().matches(&actor));
    assert!(BoardRecipient::parse("codex").unwrap().matches(&actor));
    assert!(BoardRecipient::for_actor(&actor).matches(&actor));
    assert!(!BoardRecipient::parse("claude").unwrap().matches(&actor));
    assert!(
        !BoardRecipient::parse("josh@desk/codex/c1")
            .unwrap()
            .matches(&actor)
    );
}
