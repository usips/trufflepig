use super::*;
use crate::{
    daemon::{
        RequestClass, RequestLane,
        deadline::{QueryDeadline, is_timed_out},
    },
    diagnostics::RequestContext,
    store::Store,
};

fn ask(daemon: &RootDaemon, root: &Path, cache: &Path, words: &[&str]) -> Result<String> {
    ask_by(daemon, root, cache, words, QueryDeadline::start())
}

fn ask_by(
    daemon: &RootDaemon,
    root: &Path,
    cache: &Path,
    words: &[&str],
    deadline: QueryDeadline,
) -> Result<String> {
    let mut args: Vec<String> = [
        "--no-workspace",
        "--diagnostics",
        "off",
        "--root",
        root.to_str().unwrap(),
        "--cache",
        cache.to_str().unwrap(),
    ]
    .map(str::to_owned)
    .into();
    args.extend(words.iter().map(|word| (*word).to_owned()));
    daemon.request(AcceptedRequest {
        context: RequestContext::new(None, None),
        args,
        deadline,
    })
}

#[test]
fn root_daemon_answers_index_warming_until_the_first_publication() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "pub fn warmed() {}\n").unwrap();
    let options = parse(&["--diagnostics".to_owned(), "off".to_owned()]).unwrap();
    let daemon = RootDaemon::open(root.path(), cache.path(), &options);
    let (root, cache) = (
        root.path().canonicalize().unwrap(),
        cache.path().canonicalize().unwrap(),
    );

    // Reads never index inside a request; the maintenance thread does.
    let error = ask(&daemon, &root, &cache, &["search", "sym:warmed"]).unwrap_err();
    assert!(crate::store::is_index_warming(&error), "{error:#}");
    let status: serde_json::Value =
        serde_json::from_str(&ask(&daemon, &root, &cache, &["status"]).unwrap()).unwrap();
    assert_eq!(
        (status["generation"].as_i64(), status["state"].as_str()),
        (Some(0), Some("warming"))
    );
    let read: serde_json::Value =
        serde_json::from_str(&ask(&daemon, &root, &cache, &["show", "lib.rs"]).unwrap()).unwrap();
    assert_eq!(read["lines"][0]["text"], "pub fn warmed() {}\n");

    daemon.reconcile().unwrap();
    let found: serde_json::Value =
        serde_json::from_str(&ask(&daemon, &root, &cache, &["search", "sym:warmed"]).unwrap())
            .unwrap();
    assert_eq!(found["hits"][0]["name"], "warmed", "{found}");
}

#[test]
fn root_daemon_honors_an_expired_accept_time_deadline() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "pub fn bounded() {}\n").unwrap();
    Store::open(root.path(), cache.path())
        .unwrap()
        .index()
        .unwrap();
    let options = parse(&["--diagnostics".to_owned(), "off".to_owned()]).unwrap();
    let daemon = RootDaemon::open(root.path(), cache.path(), &options);
    let root = root.path().canonicalize().unwrap();
    let cache = cache.path().canonicalize().unwrap();

    for words in [&["search", "sym:bounded"][..], &["doctor"][..]] {
        let error = ask_by(
            &daemon,
            &root,
            &cache,
            words,
            QueryDeadline::after(Duration::ZERO),
        )
        .unwrap_err();
        assert!(is_timed_out(&error), "{words:?}: {error:#}");
    }
}

#[test]
fn daemon_request_class_routes_flags_and_regex_queries_to_controlled_lanes() {
    let class = |args: &[&str]| {
        classify_daemon_request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
            .unwrap()
            .class
    };

    assert_eq!(
        class(&["--root", ".", "--budget", "300", "search", "symbol"]),
        RequestClass {
            lane: RequestLane::Query,
            verb: "search",
        }
    );
    assert_eq!(
        class(&["search", "re:entity_transfer|sim.entity.transfer"]),
        RequestClass {
            lane: RequestLane::Regex,
            verb: "search",
        }
    );
    assert_eq!(
        class(&["search", "file:src", "re:entity_transfer"]),
        RequestClass {
            lane: RequestLane::Regex,
            verb: "search",
        }
    );
    assert_eq!(
        class(&["search", "structure:foo"]),
        RequestClass {
            lane: RequestLane::Query,
            verb: "search",
        }
    );
    assert_eq!(
        class(&["hist-index", "--wait"]),
        RequestClass {
            lane: RequestLane::Maintenance,
            verb: "hist-index",
        }
    );
    for verb in [
        "hist",
        "since",
        "diff",
        "blame",
        "hist-status",
        "doctor",
        "session",
        "audit",
    ] {
        assert_eq!(
            class(&[verb]),
            RequestClass {
                lane: RequestLane::Query,
                verb,
            }
        );
    }
    assert_eq!(
        class(&["semantic", "status"]),
        RequestClass {
            lane: RequestLane::Query,
            verb: "semantic",
        }
    );
    assert_eq!(
        class(&["semantic", "prepare"]),
        RequestClass {
            lane: RequestLane::Maintenance,
            verb: "semantic",
        }
    );
    assert_eq!(
        class(&["hold"]),
        RequestClass {
            lane: RequestLane::Query,
            verb: "unknown",
        }
    );
    for (args, verb) in [
        (&["board", "show", "P7"][..], "board"),
        (&["feedback", "ls", "--open"][..], "feedback"),
        (&["system", "status"][..], "system"),
    ] {
        assert_eq!(
            class(args),
            RequestClass {
                lane: RequestLane::Board,
                verb,
            }
        );
    }
}

fn workspace_classifier_fixture(
    semantic: &str,
) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let scratch = tempfile::tempdir().unwrap();
    for member in ["engine", "pack"] {
        std::fs::create_dir(scratch.path().join(member)).unwrap();
    }
    let config = scratch.path().join("trufflepig.workspace.toml");
    std::fs::write(
        &config,
        format!(
            "[workspace]\nname='classification'\n{semantic}[members.engine]\npath='engine'\n[members.pack]\npath='pack'\n"
        ),
    )
    .unwrap();
    let engine = scratch.path().join("engine");
    (scratch, config, engine)
}

fn classify_workspace(
    config: &std::path::Path,
    root: &std::path::Path,
    words: &[&str],
) -> crate::cli::ClassifiedDaemonRequest {
    let mut args = vec![
        "--workspace".to_owned(),
        config.display().to_string(),
        "--root".to_owned(),
        root.display().to_string(),
    ];
    args.extend(words.iter().map(|word| (*word).to_owned()));
    classify_daemon_request(&args).unwrap()
}

#[test]
fn workspace_selector_position_preserves_lane_and_reserved_roots() {
    let (_scratch, config, engine) = workspace_classifier_fixture("");
    let prefix = classify_workspace(&config, &engine, &["search", "ws:all", "re:needle"]);
    let suffix = classify_workspace(&config, &engine, &["search", "re:needle", "ws:all"]);
    assert_eq!(prefix.class.lane, RequestLane::Regex);
    assert_eq!(suffix.class.lane, RequestLane::Regex);
    assert_eq!(prefix.roots, suffix.roots);
    assert_eq!(prefix.roots.len(), 2);

    let selected_prefix = classify_workspace(&config, &engine, &["search", "in:pack", "re:needle"]);
    let selected_suffix = classify_workspace(&config, &engine, &["search", "re:needle", "in:pack"]);
    assert_eq!(selected_prefix.class.lane, RequestLane::Regex);
    assert_eq!(selected_prefix.roots, selected_suffix.roots);
    assert_eq!(
        selected_prefix.roots,
        [config.parent().unwrap().join("pack")]
    );
}

#[test]
fn persistent_semantic_and_rerank_settings_use_the_semantic_lane() {
    let (_scratch, config, engine) =
        workspace_classifier_fixture("[semantic]\nenabled=true\nrerank=true\n");
    assert_eq!(
        classify_workspace(&config, &engine, &["search", "needle"])
            .class
            .lane,
        RequestLane::Semantic
    );
    assert_eq!(
        classify_workspace(&config, &engine, &["--no-sem", "search", "needle"])
            .class
            .lane,
        RequestLane::Semantic,
        "workspace rerank remains effective when semantic retrieval is disabled"
    );
    assert_eq!(
        classify_workspace(
            &config,
            &engine,
            &["--no-sem", "--no-rerank", "search", "needle"]
        )
        .class
        .lane,
        RequestLane::Query
    );
    assert_eq!(
        classify_workspace(&config, &engine, &["search", "sym:needle"])
            .class
            .lane,
        RequestLane::Query
    );
    let filter_only = classify_workspace(
        &config,
        &engine,
        &["search", "kind:file", "file:reads_item_state.rs"],
    );
    assert_eq!(filter_only.class.lane, RequestLane::Query);
    assert!(filter_only.roots.is_empty());
    let file_concept = classify_workspace(
        &config,
        &engine,
        &["search", "authentication", "flow", "kind:file"],
    );
    assert_eq!(file_concept.class.lane, RequestLane::Semantic);
    assert_eq!(file_concept.roots.len(), 2);
    let no_cache = classify_workspace(&config, &engine, &["--no-cache", "search", "needle"]);
    assert_eq!(no_cache.class.lane, RequestLane::Query);
    assert!(no_cache.roots.is_empty());
    let no_cache_regex =
        classify_workspace(&config, &engine, &["--no-cache", "search", "re:needle"]);
    assert_eq!(no_cache_regex.class.lane, RequestLane::Regex);
    assert_eq!(no_cache_regex.roots.len(), 2);
    assert_eq!(
        classify_workspace(&config, &engine, &["search", "re:needle"])
            .class
            .lane,
        RequestLane::Regex
    );
}

#[test]
fn semantic_prepare_reserves_every_effective_member_root_unless_selected() {
    let scratch = tempfile::tempdir().unwrap();
    let home_a = scratch.path().join("home-a");
    let home_b = scratch.path().join("home-b");
    let shared = scratch.path().join("shared");
    for root in [&home_a, &home_b, &shared] {
        std::fs::create_dir(root).unwrap();
    }
    let workspace_a = scratch.path().join("workspace-a.toml");
    let workspace_b = scratch.path().join("workspace-b.toml");
    for (config, name, home) in [
        (&workspace_a, "first", "home-a"),
        (&workspace_b, "second", "home-b"),
    ] {
        std::fs::write(
            config,
            format!(
                "[workspace]\nname='{name}'\n[members.home]\npath='{home}'\n[members.engine]\npath='shared'\n"
            ),
        )
        .unwrap();
    }

    let first = classify_workspace(&workspace_a, &home_a, &["semantic", "prepare"]);
    let second = classify_workspace(&workspace_b, &home_b, &["semantic", "prepare"]);
    let shared = shared.canonicalize().unwrap();
    let home_a = home_a.canonicalize().unwrap();
    let home_b = home_b.canonicalize().unwrap();
    assert_eq!(first.class.lane, RequestLane::Maintenance);
    assert_eq!(second.class.lane, RequestLane::Maintenance);
    assert!(first.roots.contains(&shared));
    assert!(second.roots.contains(&shared));
    assert!(first.roots.contains(&home_a));
    assert!(second.roots.contains(&home_b));
    assert!(!first.roots.contains(&home_b));
    assert!(!second.roots.contains(&home_a));

    let selected = classify_workspace(
        &workspace_a,
        &home_a,
        &["--member", "engine", "semantic", "prepare"],
    );
    assert_eq!(selected.roots, [shared]);

    let ordinary_owner = classify_workspace(&workspace_a, &home_a, &["index"]);
    assert_eq!(ordinary_owner.roots, [home_a]);
}
