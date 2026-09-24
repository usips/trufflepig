use super::*;
use crate::{cli, output::OutputBudget, results, store::Store};
use std::{fs, process::Command};

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

struct Fixture {
    root: tempfile::TempDir,
    cache: tempfile::TempDir,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        for member in ["engine", "pack", "upstream"] {
            fs::create_dir(root.path().join(member)).unwrap();
            fs::write(
                root.path().join(member).join("lib.rs"),
                format!("pub struct SharedThing {{ pub {member}_marker: u8 }}\n"),
            )
            .unwrap();
        }
        let config = root.path().join("trufflepig.workspace.toml");
        let fixture = Self {
            root,
            cache,
            config,
        };
        fixture.configure(&["engine", "pack", "upstream"]);
        fixture
    }

    /// Members become Git repositories with one commit each, so linked
    /// worktrees can be created from them.
    fn git_backed() -> Self {
        let fixture = Self::new();
        for member in ["engine", "pack", "upstream"] {
            let root = fixture.root.path().join(member);
            git(&root, &["init", "-q", "-b", "main"]);
            git(&root, &["add", "."]);
            git(&root, &["commit", "-q", "-m", "init"]);
        }
        fixture
    }

    /// Adds a detached linked worktree of `member` at `path`.
    fn worktree(&self, member: &str, path: &Path) -> PathBuf {
        git(
            &self.root.path().join(member),
            &["worktree", "add", "--detach", path.to_str().unwrap()],
        );
        path.canonicalize().unwrap()
    }

    fn run_at(&self, root: &Path, words: &[&str]) -> Result<String> {
        let mut args = vec![
            "--workspace".into(),
            self.config.display().to_string(),
            "--root".into(),
            root.display().to_string(),
            "--cache".into(),
            self.cache.path().display().to_string(),
            "--no-daemon".into(),
            "--diagnostics".into(),
            "off".into(),
        ];
        args.extend(words.iter().map(|word| (*word).to_owned()));
        cli::run(&args)
    }

    fn configure(&self, members: &[&str]) {
        let mut text = "[workspace]\nname = 'test-workspace'\n".to_owned();
        for member in members {
            text.push_str(&format!("[members.{member}]\npath = '{member}'\n"));
        }
        fs::write(&self.config, text).unwrap();
    }

    fn run(&self, home: &str, words: &[&str]) -> Result<String> {
        let mut args = vec![
            "--workspace".into(),
            self.config.display().to_string(),
            "--root".into(),
            self.root.path().join(home).display().to_string(),
            "--cache".into(),
            self.cache.path().display().to_string(),
            "--no-daemon".into(),
            "--diagnostics".into(),
            "off".into(),
        ];
        args.extend(words.iter().map(|word| (*word).to_owned()));
        cli::run(&args)
    }

    fn json(&self, home: &str, words: &[&str]) -> Value {
        serde_json::from_str(&self.run(home, words).unwrap()).unwrap()
    }

    fn member_cache(&self, name: &str) -> PathBuf {
        let config = WorkspaceConfig::load(&self.config).unwrap();
        let member = config
            .members
            .iter()
            .find(|member| member.name == name)
            .unwrap();
        super::member_cache(&MemberRoot::configured(member), Some(self.cache.path())).unwrap()
    }

    fn handle(&self, member: &str) -> String {
        self.json(
            "engine",
            &["search", "sym:SharedThing", &format!("in:{member}")],
        )["hits"][0]["handle"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

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
    assert_eq!(home["scope"], "home; ws:all adds 2 members");
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
        lines.contains("coverage: engine complete; scope home; ws:all adds 2 members"),
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
    results::initialize(&store).unwrap();
    store.conn.execute("DELETE FROM result_sets", []).unwrap();
    drop(store);
    let second = fixture.json("upstream", &["show", cursor]);
    assert_eq!(second["member"], "pack");
    assert_eq!(second["revision"], first["revision"]);
    assert_eq!(second["end"], first["end"]);
    assert!(source_text(&second).contains("original pack"));
    let bad_offset = format!("{}@0", cursor.rsplit_once('@').unwrap().0);
    assert!(
        fixture
            .run("engine", &["show", &bad_offset])
            .unwrap_err()
            .to_string()
            .contains("invalid_cursor")
    );
    fs::write(
        fixture.root.path().join("pack/lib.rs"),
        "// newer pack source\n",
    )
    .unwrap();
    assert!(
        fixture
            .run("engine", &["show", cursor])
            .unwrap_err()
            .to_string()
            .contains("stale_source")
    );
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
    let lines: Vec<_> = output.lines().collect();
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
