use super::*;
use std::{fs, time::Duration};
use tp_core::coded_error::CodedContext;

fn words(arguments: &[&str]) -> Vec<String> {
    arguments.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn fallback_admits_only_current_path_show_targets() {
    for target in ["path:src/lib.rs", "src/lib.rs", "read:path:src/lib.rs@120"] {
        let options = local_path_show_fallback_options(&words(&["show", target]))
            .unwrap_or_else(|| panic!("expected current path target to be eligible: {target}"));
        assert!(options.no_daemon);
    }

    let ineligible = [
        words(&["show", "sym:ClientCommand"]),
        words(&["show", "read:result-handle@120"]),
        words(&["show", "8ec51b6c-ec5a-4f97-8fc0-dc90d4e1941a:1"]),
        words(&["more", "8ec51b6c-ec5a-4f97-8fc0-dc90d4e1941a:1"]),
        words(&["ctx", "8ec51b6c-ec5a-4f97-8fc0-dc90d4e1941a:1"]),
        words(&["index"]),
        words(&["board", "claim", "P11.5"]),
    ];
    for arguments in &ineligible {
        assert!(
            local_path_show_fallback_options(arguments).is_none(),
            "unexpected fallback eligibility for {arguments:?}"
        );
    }
}

#[test]
fn fallback_preserves_root_cache_workspace_member_and_identity_options() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace.toml");
    fs::write(&workspace, "[workspace]\n").unwrap();
    let arguments = vec![
        "--root".to_owned(),
        root.path().display().to_string(),
        "--cache".to_owned(),
        cache.path().display().to_string(),
        "--workspace".to_owned(),
        workspace.display().to_string(),
        "--member".to_owned(),
        "pack".to_owned(),
        "--client".to_owned(),
        "codex".to_owned(),
        "--session".to_owned(),
        "fallback-session".to_owned(),
        "show".to_owned(),
        "path:src/lib.rs".to_owned(),
    ];

    let options = local_path_show_fallback_options(&arguments).unwrap();
    assert_eq!(options.root.as_path(), root.path());
    assert_eq!(options.cache.as_deref(), Some(cache.path()));
    assert_eq!(options.workspace.as_deref(), Some(workspace.as_path()));
    assert_eq!(options.member.as_deref(), Some("pack"));
    assert_eq!(options.client.as_deref(), Some("codex"));
    assert_eq!(options.session.as_deref(), Some("fallback-session"));
    assert!(options.no_daemon);
}

#[test]
fn exhausted_busy_fallback_reads_current_source_from_the_requested_root() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let source = root.path().join("src/lib.rs");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "pub fn fallback_current_bytes() {}\n").unwrap();
    let blocked_cache = cache.path().join("explicit-cache");
    fs::write(&blocked_cache, "preserve this cache path").unwrap();
    let arguments = vec![
        "--root".to_owned(),
        root.path().display().to_string(),
        "--cache".to_owned(),
        blocked_cache.display().to_string(),
        "show".to_owned(),
        "path:src/lib.rs".to_owned(),
    ];
    let context = tp_core::request_context::RequestContext::new(
        Some("fallback-session".to_owned()),
        Some("codex".to_owned()),
    );

    let mut command = ClientCommand::default();
    let response = command
        .run_after_busy_exhaustion(
            &arguments,
            &context,
            tp_core::query_deadline::QueryDeadline::after(Duration::from_secs(5)),
        )
        .expect("current path show should be eligible")
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["source"], "current_file");
    assert_eq!(response["verified"], false);
    let revision =
        tp_core::identity::ContentRevision::of(b"pub fn fallback_current_bytes() {}\n").to_string();
    assert_eq!(response["revision"].as_str(), Some(revision.as_str()));
    assert_eq!(response["path"], "src/lib.rs");
    assert!(response.to_string().contains("fallback_current_bytes"));
    assert!(blocked_cache.is_file());
    assert_eq!(
        fs::read_to_string(blocked_cache).unwrap(),
        "preserve this cache path"
    );
}

#[test]
fn current_path_show_survives_refused_daemon_cache_creation() {
    let root = tempfile::tempdir().unwrap();
    let cache_parent = tempfile::tempdir().unwrap();
    let source = root.path().join("src/lib.rs");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "pub fn cache_refusal_bytes() {}\n").unwrap();
    let blocked_cache = cache_parent.path().join("cache-file");
    fs::write(&blocked_cache, "preserve this file").unwrap();
    let arguments = vec![
        "--root".to_owned(),
        root.path().display().to_string(),
        "--cache".to_owned(),
        blocked_cache.display().to_string(),
        "show".to_owned(),
        "path:src/lib.rs".to_owned(),
    ];
    let options = parse(&arguments).unwrap();
    let context = tp_core::request_context::RequestContext::new(
        Some("startup-fallback".to_owned()),
        Some("codex".to_owned()),
    );

    let response = super::super::direct_with_deadline(
        &options,
        &context,
        tp_core::query_deadline::QueryDeadline::after(Duration::from_secs(5)),
    )
    .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["source"], "current_file");
    assert_eq!(response["verified"], false);
    let revision =
        tp_core::identity::ContentRevision::of(b"pub fn cache_refusal_bytes() {}\n").to_string();
    assert_eq!(response["revision"].as_str(), Some(revision.as_str()));
    assert_eq!(response["path"], "src/lib.rs");
    assert!(response.to_string().contains("cache_refusal_bytes"));
    assert!(blocked_cache.is_file());
    assert_eq!(
        fs::read_to_string(blocked_cache).unwrap(),
        "preserve this file"
    );
}

#[test]
fn exhausted_busy_fallback_keeps_the_original_expired_deadline() {
    let root = tempfile::tempdir().unwrap();
    let arguments = vec![
        "--root".to_owned(),
        root.path().display().to_string(),
        "show".to_owned(),
        "path:missing.rs".to_owned(),
    ];
    let context = tp_core::request_context::RequestContext::new(None, None);

    let mut command = ClientCommand::default();
    let error = command
        .run_after_busy_exhaustion(
            &arguments,
            &context,
            tp_core::query_deadline::QueryDeadline::after(Duration::ZERO),
        )
        .expect("current path show should be eligible")
        .unwrap_err();
    assert!(ErrorCode::TimedOut.is_code_of(&error), "{error:#}");
}

#[test]
fn cache_refusal_fallback_requires_an_exact_code_and_current_path_show() {
    let accepted =
        relayed("daemon: daemon: index_cache_unavailable: selected cache cannot be opened");
    let eligible = words(&["show", "path:src/lib.rs"]);
    let options = index_cache_fallback_options(&eligible, &accepted)
        .expect("exact relayed cache refusal should admit current-path show");
    assert!(options.no_daemon);

    for message in [
        "invalid_body: mentions index_cache_unavailable but is not a cache refusal",
        "diagnostic: index_cache_unavailable appeared in a diagnostic",
        "timed_out: index_cache_unavailable appeared in a timeout detail",
        "daemon_reply_timeout: index_cache_unavailable appeared in a timeout detail",
    ] {
        let error = relayed(message);
        assert!(
            index_cache_fallback_options(&eligible, &error).is_none(),
            "unexpected fallback for {message}"
        );
    }

    let nested_timeout = anyhow::anyhow!(coded!(IndexCacheUnavailable, "cause"))
        .with_coded_context(|| coded!(TimedOut, "enclosing request expired"));
    assert!(index_cache_fallback_options(&eligible, &nested_timeout).is_none());

    let typed_timeout = anyhow::Error::new(tp_core::daemon_transport::DaemonReplyTimeout::new(
        Duration::from_millis(5),
        Duration::from_millis(5),
    ))
    .with_coded_context(|| coded!(IndexCacheUnavailable, "misleading wrapper"));
    assert!(index_cache_fallback_options(&eligible, &typed_timeout).is_none());

    let non_path = words(&["show", "sym:ClientCommand"]);
    assert!(
        index_cache_fallback_options(&non_path, &accepted).is_none(),
        "cache refusal must not route symbol reads through the local current-file path"
    );
}

/// Types reply text as the daemon client boundary does.
fn relayed(text: &str) -> anyhow::Error {
    tp_core::coded_error::relayed_error(text.to_owned())
}

#[test]
fn cache_refusal_fallback_reads_every_layer() {
    let eligible = words(&["show", "path:src/lib.rs"]);
    let wrapped = anyhow::anyhow!(coded!(IndexCacheUnavailable, "cache cannot be opened"))
        .with_coded_context(|| coded!(MemberUnavailable, "member read failed"));
    assert!(index_cache_fallback_options(&eligible, &wrapped).is_some());

    let inner_timeout = anyhow::anyhow!(coded!(TimedOut, "query deadline expired"))
        .with_coded_context(|| coded!(IndexCacheUnavailable, "cache wrapper"));
    assert!(index_cache_fallback_options(&eligible, &inner_timeout).is_none());
    let inner_reply_timeout = anyhow::anyhow!(coded!(DaemonReplyTimeout, "no reply"))
        .with_coded_context(|| coded!(IndexCacheUnavailable, "cache wrapper"));
    assert!(index_cache_fallback_options(&eligible, &inner_reply_timeout).is_none());
}

#[test]
fn an_unavailable_owner_named_anywhere_leaves_the_router() {
    for error in [
        relayed("daemon: daemon_unavailable: no socket"),
        relayed("daemon: member_unavailable: owner refused: daemon_unavailable: no socket"),
        relayed("route request: daemon_unavailable: owner stopped"),
        anyhow::anyhow!(coded!(DaemonUnavailable, "no socket"))
            .with_coded_context(|| coded!(SystemUnavailable, "router relay")),
    ] {
        assert!(leaves_router_for_direct_execution(&error), "{error:#}");
    }
    for error in [
        relayed("daemon: invalid_query: empty prefixed query"),
        relayed("daemon: member_unavailable: the daemon is unavailable"),
    ] {
        assert!(!leaves_router_for_direct_execution(&error), "{error:#}");
    }
}

#[test]
fn a_replaced_binary_refusal_uses_bounded_retrieval_and_keeps_writes_refused() {
    let refusals = [
        relayed("daemon: router_binary_replaced: executable /opt/trufflepig (deleted)"),
        relayed(
            "daemon: member_unavailable: owner refused: router_binary_replaced: executable /opt/trufflepig (deleted)",
        ),
    ];
    for read in [
        &["show", "sym:ClientCommand"][..],
        &["more", "8ec51b6c-ec5a-4f97-8fc0-dc90d4e1941a:1"][..],
        &["ctx", "8ec51b6c-ec5a-4f97-8fc0-dc90d4e1941a:1"][..],
        &["status"][..],
    ] {
        let options = parse(&words(read)).unwrap();
        for refusal in &refusals {
            let local = replaced_binary_read_options(&options, refusal)
                .unwrap_or_else(|| panic!("{read:?} should run locally after {refusal:#}"));
            assert!(local.no_daemon);
            assert!(!local.no_cache);
            assert_eq!(local.words, options.words);
        }
        let other = relayed("daemon: invalid_query: empty prefixed query");
        assert!(replaced_binary_read_options(&options, &other).is_none());
    }
    for read in [
        &["search", "sym:ClientCommand"][..],
        &["refs", "ClientCommand"][..],
        &["map", "src/cli.rs"][..],
    ] {
        let options = parse(&words(read)).unwrap();
        for refusal in &refusals {
            let local = replaced_binary_read_options(&options, refusal)
                .unwrap_or_else(|| panic!("{read:?} should use current-source recovery"));
            assert!(local.no_cache);
            assert!(!local.no_daemon);
            assert_eq!(local.words, options.words);
        }
    }
    for write in [
        &["hist-index"][..],
        &["semantic", "prepare"][..],
        &["board", "claim", "P11.5"][..],
        &["feedback", "new", "title"][..],
    ] {
        let options = parse(&words(write)).unwrap();
        assert!(
            replaced_binary_read_options(&options, &refusals[0]).is_none(),
            "{write:?} keeps the refusal"
        );
    }
}
