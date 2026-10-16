use super::test_fixture::{Fixture, git};
use super::*;
use crate::{cli, output::OutputBudget, store::Store};
use std::fs;

mod query_execution_deadline_tests;

fn source_text(value: &Value) -> String {
    value["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["text"].as_str().unwrap())
        .collect()
}

#[test]
fn default_budget_has_fair_order_and_complete_member_provenance() {
    let fixture = Fixture::new();
    let budget = OutputBudget::new(600).unwrap();
    let output = fixture
        .run("pack", &["search", "sym:SharedThing", "ws:all"])
        .unwrap();
    assert!(budget.fits(&output));
    let mut page: Value = serde_json::from_str(&output).unwrap();
    let mut members = Vec::new();
    loop {
        assert_eq!(page["home"], "pack");
        assert_eq!(page["coverage"].as_array().unwrap().len(), 3);
        for coverage in page["coverage"].as_array().unwrap() {
            assert_eq!(coverage["state"], "searched");
        }
        for hit in page["hits"].as_array().unwrap() {
            let member = hit["member"].as_str().unwrap();
            assert_eq!(
                hit["file"],
                format!(
                    "file://{}/lib.rs",
                    encode_path(&fixture.root.path().join(member))
                )
            );
            assert!(hit["start_line"].as_u64().is_some());
            assert!(hit["handle"].as_str().is_some());
            members.push(member.to_owned());
        }
        let Some(next) = page["next"].as_str() else {
            break;
        };
        page = fixture.json("upstream", &["more", next]);
    }
    assert_eq!(members, ["pack", "engine", "upstream"]);
}

#[test]
fn unselected_queries_search_home_and_widen_without_home_hits() {
    let fixture = Fixture::new();
    let home = fixture.json("engine", &["search", "sym:SharedThing"]);
    let members: Vec<_> = home["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| &hit["member"])
        .collect();
    assert_eq!(members, ["engine"]);
    assert_eq!(home["coverage"].as_array().unwrap().len(), 1);
    assert_eq!(home["scope"], "home (ws:all adds 2 members)");
    let widened = fixture.json("engine", &["search", "re:pack_marker"]);
    assert_eq!(widened["hits"][0]["member"], "pack");
    assert_eq!(widened["scope"], "all (no home hits)");
    let lines = fixture
        .run(
            "engine",
            &["--format", "lines", "search", "sym:SharedThing"],
        )
        .unwrap();
    assert!(
        lines.contains("coverage: engine complete; scope home (ws:all adds 2 members)"),
        "{lines}"
    );
    let explicit = fixture.json("engine", &["search", "sym:SharedThing", "ws:home"]);
    assert!(explicit["scope"].is_null());
}

#[test]
fn unknown_workspace_command_is_rejected() {
    let fixture = Fixture::new();
    let error = fixture.run("engine", &["not-a-command"]).unwrap_err();
    assert!(error.to_string().contains("unknown_command"));
    assert!(error.to_string().contains("not-a-command"));
}

#[test]
fn duplicate_paths_route_show_and_context_to_the_recorded_member() {
    let fixture = Fixture::new();
    let handle = fixture.handle("pack");
    let shown = fixture.json("upstream", &["show", &handle]);
    assert_eq!(shown["member"], "pack");
    assert_eq!(
        shown["repository"],
        encode_path(&fixture.root.path().join("pack"))
    );
    assert!(source_text(&shown).contains("pack_marker"));
    assert!(!source_text(&shown).contains("engine_marker"));
    let context = fixture.json("engine", &["ctx", &handle]);
    assert_eq!(context["member"], "pack");
    assert_eq!(context["hit"]["handle"], handle);
    assert!(
        fixture
            .run("engine", &["--member", "engine", "show", &handle])
            .unwrap_err()
            .to_string()
            .contains("invalid_member")
    );
}

#[test]
fn pagination_keeps_original_order_when_the_home_member_changes() {
    let fixture = Fixture::new();
    let first = fixture.json(
        "upstream",
        &["--limit", "1", "search", "sym:SharedThing", "ws:all"],
    );
    assert_eq!(first["hits"][0]["member"], "upstream");
    let second = fixture.json(
        "pack",
        &["--limit", "1", "more", first["next"].as_str().unwrap()],
    );
    assert_eq!(second["hits"][0]["member"], "engine");
    assert_eq!(second["home"], "upstream");
    let third = fixture.json(
        "engine",
        &["--limit", "1", "more", second["next"].as_str().unwrap()],
    );
    assert_eq!(third["hits"][0]["member"], "pack");
    assert!(third["next"].is_null());
}

#[test]
fn explicit_scopes_limit_members_and_unavailable_members_remain_visible() {
    let fixture = Fixture::new();
    let scoped = fixture.json("engine", &["search", "sym:SharedThing", "in:upstream"]);
    assert_eq!(scoped["hits"].as_array().unwrap().len(), 1);
    assert_eq!(scoped["hits"][0]["member"], "upstream");
    assert_eq!(scoped["coverage"].as_array().unwrap().len(), 1);
    let home = fixture.json("pack", &["search", "sym:SharedThing", "ws:home"]);
    assert_eq!(home["hits"][0]["member"], "pack");
    fs::remove_dir_all(fixture.root.path().join("upstream")).unwrap();
    let partial = fixture.json("engine", &["search", "sym:SharedThing", "ws:all"]);
    assert!(
        partial["coverage"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["member"] == "upstream" && row["state"] == "unavailable")
    );
    assert!(
        !partial["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["member"] == "upstream")
    );
    assert!(
        fixture
            .run("engine", &["search", "sym:SharedThing", "in:upstream"])
            .unwrap_err()
            .to_string()
            .contains("workspace_unavailable")
    );
}

/// Runs `search sym:SharedThing ws:all` (`--no-daemon`) from `engine` with a fresh deadline.
fn search_all_with_deadline(
    fixture: &Fixture,
    deadline_factory: impl FnOnce() -> crate::daemon::deadline::QueryDeadline,
) -> Result<String> {
    let workspace_path = fixture
        .config
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("workspace config path is not valid UTF-8"))?;
    let engine_root = fixture.root.path().join("engine");
    let engine_root = engine_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("engine root path is not valid UTF-8"))?;
    let cache_path_arg = fixture
        .cache
        .path()
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("cache path is not valid UTF-8"))?;
    let args: Vec<String> = [
        "--workspace",
        workspace_path,
        "--root",
        engine_root,
        "--cache",
        cache_path_arg,
        "--no-daemon",
        "--diagnostics",
        "off",
        "search",
        "sym:SharedThing",
        "ws:all",
    ]
    .map(str::to_owned)
    .into();
    let options = cli::parse(&args)?;
    let config = WorkspaceConfig::load(&fixture.config)?;
    let cache = cache_path(&config, Some(fixture.cache.path()))?;
    let setup_deadline =
        crate::daemon::deadline::QueryDeadline::after(std::time::Duration::from_secs(5));
    WorkspaceResults::create(&cache, &setup_deadline)?;
    let request_context = cli::request_context(&options)?;
    let deadline = deadline_factory();
    let results = WorkspaceResults::open(&cache, &deadline)?;
    retrieval::search(
        &config,
        &cache,
        &results,
        &options,
        &request_context,
        &mut crate::semantic::SemanticSession::default(),
        deadline,
    )
}

#[test]
fn expired_query_deadline_answers_timed_out() {
    let fixture = Fixture::new();
    let mut deadline_factory_invoked = false;
    let error = search_all_with_deadline(&fixture, || {
        deadline_factory_invoked = true;
        crate::daemon::deadline::QueryDeadline::after(std::time::Duration::ZERO)
    })
    .unwrap_err();
    assert!(deadline_factory_invoked);
    assert!(crate::daemon::deadline::is_timed_out(&error), "{error:#}");
}

#[test]
fn member_slices_end_before_the_original_budget_and_later_members_get_the_remainder() {
    let fixture = Fixture::new();
    let mut selected_caches = Vec::with_capacity(3);
    let mut _writer_leases = Vec::with_capacity(3);
    for member in ["engine", "pack", "upstream"] {
        let root = fixture.root.path().join(member);
        let cache = super::hashed_member_cache(&root, Some(fixture.cache.path())).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let lease = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(cache.join("index.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lease).unwrap();
        selected_caches.push((root, cache));
        _writer_leases.push(lease);
    }

    // Each member's slice is capped at the original deadline minus 250 ms of headroom,
    // so the first member takes the remainder and later members get little or none.
    let budget = std::time::Duration::from_millis(750);
    let mut started = None;
    let mut original_deadline = None;
    let error = search_all_with_deadline(&fixture, || {
        let deadline = crate::daemon::deadline::QueryDeadline::after(budget);
        started = Some(std::time::Instant::now());
        original_deadline = Some(deadline.clone());
        deadline
    })
    .unwrap_err();
    let elapsed = started.expect("deadline factory ran").elapsed();
    let original_deadline = original_deadline.expect("original deadline was captured");
    // Under load the first member's slice can expire before it answers, which reports
    // `timed_out` instead of `workspace_unavailable`.
    let unavailable = error.to_string()
        == "workspace_unavailable: no selected member has an available published index; inspect ws status or use --no-daemon";
    assert!(
        unavailable || crate::daemon::deadline::is_timed_out(&error),
        "{error:#}"
    );
    assert!(
        !original_deadline.expired(),
        "member warming leaves the original response headroom: {error:#}"
    );
    assert!(
        elapsed < budget,
        "member slices end before the original budget: {elapsed:?} vs {budget:?}"
    );

    for (index, (root, cache)) in selected_caches.iter().enumerate() {
        let database = cache.join("index.sqlite3");
        if index == 0 {
            assert!(
                !unavailable || database.is_file(),
                "first member setup ran: {database:?}"
            );
        } else {
            assert!(
                !database.exists(),
                "later member got a renewed slice: {database:?}"
            );
        }
        let error = Store::open_read(
            root,
            cache,
            crate::daemon::deadline::QueryDeadline::after(std::time::Duration::from_millis(100)),
        )
        .err()
        .expect("every writer-locked member remains unpublished");
        assert!(
            crate::store::is_index_warming(&error),
            "published read remains warming for {root:?}: {error:#}"
        );
    }
}

#[test]
fn removed_and_replaced_members_cannot_retarget_retained_handles() {
    let fixture = Fixture::new();
    let handle = fixture.handle("pack");
    fixture.configure(&["engine", "upstream"]);
    assert!(
        fixture
            .run("engine", &["show", &handle])
            .unwrap_err()
            .to_string()
            .contains("member_unavailable")
    );
    fixture.configure(&["engine", "pack", "upstream"]);
    let original = fixture.root.path().join("pack");
    fs::rename(&original, fixture.root.path().join("old-pack")).unwrap();
    fs::create_dir(&original).unwrap();
    fs::write(
        original.join("lib.rs"),
        "pub struct SharedThing { pub replacement: u8 }\n",
    )
    .unwrap();
    assert!(
        fixture
            .run("engine", &["show", &handle])
            .unwrap_err()
            .to_string()
            .contains("member_unavailable")
    );
}

#[test]
fn changed_source_and_republished_graph_reject_old_workspace_handles() {
    let fixture = Fixture::new();
    let handle = fixture.handle("pack");
    fs::write(
        fixture.root.path().join("pack/lib.rs"),
        "struct Replacement {}\n",
    )
    .unwrap();
    assert!(
        fixture
            .run("engine", &["show", &handle])
            .unwrap_err()
            .to_string()
            .contains("stale_source")
    );
    fixture.json("engine", &["search", "sym:Replacement", "in:pack"]);
    assert!(
        fixture
            .run("upstream", &["ctx", &handle])
            .unwrap_err()
            .to_string()
            .contains("stale_result")
    );
}

#[test]
fn explicit_read_continuations_survive_member_cache_eviction_and_pin_range() {
    let fixture = Fixture::new();
    let body = (0..220)
        .map(|line| format!("// original pack line {line}\n"))
        .collect::<String>();
    fs::write(fixture.root.path().join("pack/lib.rs"), &body).unwrap();
    let first = fixture.json("engine", &["--member", "pack", "show", "path:lib.rs:2-215"]);
    let cursor = first["next"].as_str().unwrap();
    let store = Store::open(
        &fixture.root.path().join("pack"),
        &fixture.member_cache("pack"),
    )
    .unwrap();
    store
        .result_sets()
        .unwrap()
        .conn
        .execute("DELETE FROM result_sets", [])
        .unwrap();
    drop(store);
    let second = fixture.json("upstream", &["--member", "pack", "show", cursor]);
    assert_eq!(second["member"], "pack");
    assert_eq!(second["source"], "current_file", "{second}");
    assert_eq!(second["verified"], false, "{second}");
    assert_eq!(second["revision"], first["revision"]);
    let cursor_offset = cursor.rsplit_once('@').unwrap().1.parse::<u64>().unwrap();
    assert_eq!(second["start"], cursor_offset, "{second}");
    assert_eq!(second["end"], first["end"]);
    assert!(source_text(&second).contains("original pack"));
    let bad_offset = format!("{}@0", cursor.rsplit_once('@').unwrap().0);
    assert!(
        fixture
            .run("engine", &["--member", "pack", "show", &bad_offset])
            .unwrap_err()
            .to_string()
            .contains("invalid_cursor")
    );
    fs::write(
        fixture.root.path().join("pack/lib.rs"),
        "// newer pack source\n",
    )
    .unwrap();
    let stale = fixture
        .json_result("engine", &["--member", "pack", "show", cursor])
        .unwrap_err()
        .to_string();
    assert!(stale.starts_with("stale_source:"), "{stale}");
}

#[test]
fn no_workspace_keeps_singleton_search_and_navigation() {
    let fixture = Fixture::new();
    let cache = tempfile::tempdir().unwrap();
    let mut args = vec![
        "--no-workspace".into(),
        "--no-daemon".into(),
        "--diagnostics".into(),
        "off".into(),
        "--root".into(),
        fixture.root.path().join("engine").display().to_string(),
        "--cache".into(),
        cache.path().display().to_string(),
        "search".into(),
        "sym:SharedThing".into(),
    ];
    let page: Value = serde_json::from_str(&cli::run(&args).unwrap()).unwrap();
    assert!(page.get("workspace").is_none());
    assert_eq!(page["hits"].as_array().unwrap().len(), 1);
    args.pop();
    args.pop();
    args.extend([
        "show".into(),
        page["hits"][0]["handle"].as_str().unwrap().into(),
    ]);
    let shown: Value = serde_json::from_str(&cli::run(&args).unwrap()).unwrap();
    assert!(source_text(&shown).contains("engine_marker"));
}

#[test]
fn selector_only_queries_and_cache_root_boundaries_are_safe() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let member = dir.path().join("member");
    std::fs::create_dir(&member)?;
    std::fs::write(member.join("lib.rs"), "pub fn crayons() {}\n")?;
    let config = dir.path().join("ws.toml");
    std::fs::write(
        &config,
        "[workspace]\nname='test'\n[members.one]\npath='member'\n",
    )?;
    let mut args = vec![
        "--workspace".into(),
        config.display().to_string(),
        "--root".into(),
        member.display().to_string(),
        "--no-daemon".into(),
        "--diagnostics".into(),
        "off".into(),
        "--cache".into(),
        dir.path().join("cache").display().to_string(),
        "search".into(),
        "in:one".into(),
    ];
    let output: serde_json::Value = serde_json::from_str(&crate::cli::run(&args)?)?;
    assert_eq!(output["hits"][0]["member"], "one");
    args[8] = member.join("cache").display().to_string();
    assert!(
        crate::cli::run(&args)
            .unwrap_err()
            .to_string()
            .contains("invalid_cache:")
    );
    Ok(())
}

#[test]
fn lines_format_prefixes_member_and_summarizes_coverage() {
    let fixture = Fixture::new();
    let output = fixture
        .run(
            "pack",
            &["--format", "lines", "search", "sym:SharedThing", "ws:all"],
        )
        .unwrap();
    // Each hit line is followed by its indented snippet line.
    let snippets: Vec<_> = output
        .lines()
        .filter(|line| line.starts_with("  "))
        .collect();
    assert_eq!(snippets.len(), 3, "{output}");
    assert!(snippets.iter().all(|line| line.starts_with("  1: ")));
    let lines: Vec<_> = output
        .lines()
        .filter(|line| !line.starts_with("  "))
        .collect();
    let hits: Vec<_> = crate::output::lines::emitted_handles(&output).collect();
    assert_eq!(hits.len(), 3, "{output}");
    for (line, member) in lines.iter().zip(["pack", "engine", "upstream"]) {
        let mut fields = line.split('\t');
        fields
            .next()
            .unwrap()
            .parse::<crate::identity::ResultHandle>()
            .unwrap();
        assert_eq!(fields.next().unwrap(), format!("{member}/lib.rs:1-1"));
        assert_eq!(fields.next().unwrap(), "SharedThing");
    }
    let coverage = lines[3].strip_prefix("coverage: ").unwrap();
    for member in ["engine", "pack", "upstream"] {
        assert!(
            coverage.contains(&format!("{member} complete")),
            "{coverage}"
        );
    }
    assert!(!output.contains("next: "));
    let json = fixture.run("pack", &["search", "sym:SharedThing"]).unwrap();
    assert!(serde_json::from_str::<Value>(&json).is_ok());
}

fn worktree_substitutes_home_member_root(fixture: &Fixture, worktree: &Path) {
    let engine = fixture.root.path().join("engine");
    fs::write(worktree.join("only.rs"), "pub struct WorktreeOnlySymbol;\n").unwrap();
    fs::write(engine.join("main_only.rs"), "pub struct MainOnlySymbol;\n").unwrap();
    let label = worktree.file_name().unwrap().to_str().unwrap();
    let page: Value = serde_json::from_str(
        &fixture
            .run_at(worktree, &["search", "sym:WorktreeOnlySymbol"])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(page["home"], "engine");
    let hits = page["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{page}");
    assert_eq!(hits[0]["member"], "engine");
    assert_eq!(
        hits[0]["file"],
        format!("file://{}/only.rs", encode_path(worktree))
    );
    let coverage = page["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["member"] == "engine")
        .unwrap();
    assert_eq!(coverage["worktree"], label);
    assert_eq!(coverage["root"], encode_path(worktree));
    assert_eq!(coverage["state"], "searched");
    let lines = fixture
        .run_at(
            worktree,
            &["--format", "lines", "search", "sym:WorktreeOnlySymbol"],
        )
        .unwrap();
    assert!(
        lines.contains(&format!("coverage: engine@{label} complete")),
        "{lines}"
    );
    let main_only: Value = serde_json::from_str(
        &fixture
            .run_at(worktree, &["search", "sym:MainOnlySymbol"])
            .unwrap(),
    )
    .unwrap();
    assert!(
        main_only["hits"].as_array().unwrap().is_empty(),
        "{main_only}"
    );
    let shared: Value = serde_json::from_str(
        &fixture
            .run_at(worktree, &["search", "sym:SharedThing", "ws:all"])
            .unwrap(),
    )
    .unwrap();
    let members: Vec<_> = shared["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["member"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(members, ["engine", "pack", "upstream"]);
    let pack = fixture.root.path().join("pack").canonicalize().unwrap();
    assert_eq!(
        shared["hits"][1]["file"],
        format!("file://{}/lib.rs", encode_path(&pack))
    );
}

#[test]
fn in_repo_worktree_substitutes_home_member_root() {
    let fixture = Fixture::git_backed();
    let worktree = fixture.worktree(
        "engine",
        &fixture.root.path().join("engine/.worktrees/inside"),
    );
    worktree_substitutes_home_member_root(&fixture, &worktree);
}

#[test]
fn external_worktree_substitutes_home_member_root() {
    let fixture = Fixture::git_backed();
    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("external"));
    worktree_substitutes_home_member_root(&fixture, &worktree);
}

#[test]
fn worktree_search_scopes_ws_home_and_leaves_other_members_unchanged() {
    let fixture = Fixture::git_backed();
    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("scoped"));
    let page: Value = serde_json::from_str(
        &fixture
            .run_at(
                &worktree.join("src"),
                &["search", "ws:home sym:SharedThing"],
            )
            .unwrap_or_else(|_| {
                fixture
                    .run_at(&worktree, &["search", "ws:home sym:SharedThing"])
                    .unwrap()
            }),
    )
    .unwrap();
    let hits = page["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{page}");
    assert_eq!(hits[0]["member"], "engine");
    assert_eq!(
        hits[0]["file"],
        format!("file://{}/lib.rs", encode_path(&worktree))
    );
    let status: Value =
        serde_json::from_str(&fixture.run_at(&worktree, &["ws", "show"]).unwrap()).unwrap();
    assert_eq!(status["home"], "engine");
    let engine = status["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["member"] == "engine")
        .unwrap();
    assert_eq!(engine["root"], encode_path(&worktree));
    assert_eq!(engine["worktree"], "scoped");
}

#[test]
fn worktree_handles_reopen_until_worktree_is_removed() {
    let fixture = Fixture::git_backed();
    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("gone"));
    let page: Value = serde_json::from_str(
        &fixture
            .run_at(&worktree, &["search", "in:engine sym:SharedThing"])
            .unwrap(),
    )
    .unwrap();
    let handle = page["hits"][0]["handle"].as_str().unwrap().to_owned();
    let shown: Value =
        serde_json::from_str(&fixture.run_at(&worktree, &["show", &handle]).unwrap()).unwrap();
    assert_eq!(shown["member"], "engine");
    assert_eq!(shown["worktree"], "gone");
    assert_eq!(shown["repository"], encode_path(&worktree));
    git(
        &fixture.root.path().join("engine"),
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    let engine = fixture.root.path().join("engine").canonicalize().unwrap();
    let error = fixture
        .run_at(&engine, &["show", &handle])
        .unwrap_err()
        .to_string();
    assert!(error.contains("member_unavailable"), "{error}");
}

#[test]
fn member_navigation_reads_every_word_and_ranks_from_the_invocation_directory() {
    let fixture = Fixture::new();
    let engine = fixture.root.path().join("engine");
    for directory in ["a", "b"] {
        fs::create_dir(engine.join(directory)).unwrap();
    }
    fs::write(engine.join("a/run.rs"), "pub fn run_here() {}\n").unwrap();
    fs::write(
        engine.join("b/run.rs"),
        "pub fn run_here() {}\nfn call() { run_here(); }\n",
    )
    .unwrap();
    // `--no-daemon` `show` reads the published index, which `refs` builds first.
    let refs = fixture
        .run(
            "engine",
            &["--format", "lines", "refs", "run_here", "file:b/"],
        )
        .unwrap();
    assert_eq!(
        crate::output::lines::emitted_handles(&refs).count(),
        2,
        "{refs}"
    );
    assert!(!refs.contains("a/run.rs"), "{refs}");
    assert!(
        refs.contains("engine complete (refs 2 sites in 1 file)"),
        "{refs}"
    );
    let near_b = fixture
        .run_at(
            &engine.join("b"),
            &[
                "--implicit-root",
                "--format",
                "lines",
                "show",
                "sym:run_here",
            ],
        )
        .unwrap();
    assert!(near_b.starts_with("b/run.rs (engine) "), "{near_b}");
    let searched = fixture
        .run_at(
            &engine.join("b"),
            &[
                "--implicit-root",
                "--format",
                "lines",
                "search",
                "sym:run_here",
            ],
        )
        .unwrap();
    let first = searched.lines().next().unwrap_or_default();
    assert!(first.contains("\tengine/b/run.rs:1-1"), "{searched}");
    let filtered = fixture
        .run(
            "engine",
            &["--format", "lines", "show", "sym:run_here", "file:a/"],
        )
        .unwrap();
    assert!(filtered.starts_with("a/run.rs (engine) "), "{filtered}");
    let missing = fixture.run("engine", &["map", "nope/run.rs"]).unwrap_err();
    assert_eq!(
        missing.to_string(),
        "no_indexed_path: no indexed file under `nope/run.rs`; nearest: a/run.rs, b/run.rs"
    );
    fs::write(
        fixture.root.path().join("pack/only_pack.rs"),
        "pub fn packed() {}\n",
    )
    .unwrap();
    let widened = fixture
        .run("engine", &["--format", "lines", "map", "only_pack.rs"])
        .unwrap();
    assert!(widened.contains("\tpack/only_pack.rs:1-1"), "{widened}");
    assert!(widened.contains("engine complete"), "{widened}");
    assert!(widened.contains("no home hits"), "{widened}");
}
