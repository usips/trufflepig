use super::{HomeIndexPolicy, WorktreeDivergence};
use crate::{
    cli,
    daemon::deadline::QueryDeadline,
    store::Store,
    workspace::{
        WorkspaceConfig, cache_path, member_cache,
        member_root::MemberRoot,
        result_cache::WorkspaceResults,
        retrieval,
        test_fixture::{Fixture, git},
    },
};
use serde_json::Value;
use std::{fs, path::Path, path::PathBuf, time::Duration};

/// A worktree of `engine` whose cache is only seeded: `lib.rs` edited so
/// `SharedThing` moves down, untracked `only.rs` defining `WorktreeOnlySymbol`,
/// and committed `other.rs` unchanged. Every member is published.
struct SeededWorktree {
    fixture: Fixture,
    _elsewhere: tempfile::TempDir,
    worktree: PathBuf,
}

impl SeededWorktree {
    fn new() -> Self {
        let fixture = Fixture::git_backed();
        let engine = fixture.root.path().join("engine");
        fs::write(engine.join("other.rs"), "pub fn untouched_helper() {}\n").unwrap();
        git(&engine, &["add", "."]);
        git(&engine, &["commit", "-q", "-m", "other"]);
        fixture.json("engine", &["search", "sym:SharedThing", "ws:all"]);
        let elsewhere = tempfile::tempdir().unwrap();
        let worktree = fixture.worktree("engine", &elsewhere.path().join("wt-l1"));
        fs::write(
            worktree.join("lib.rs"),
            "// worktree edit\n\npub struct SharedThing { pub worktree_marker: u8 }\n\npub fn added_in_worktree() {}\n",
        )
        .unwrap();
        fs::write(worktree.join("only.rs"), "pub struct WorktreeOnlySymbol;\n").unwrap();
        let seeded = Self {
            fixture,
            _elsewhere: elsewhere,
            worktree,
        };
        let cache = seeded.worktree_cache();
        assert!(
            cache.join("index.sqlite3").is_file(),
            "worktree cache is seeded"
        );
        seeded
    }

    fn worktree_cache(&self) -> PathBuf {
        let config = WorkspaceConfig::load(&self.fixture.config).unwrap();
        let engine = config.members.iter().find(|m| m.name == "engine").unwrap();
        let root = MemberRoot::linked(engine, self.worktree.clone()).unwrap();
        member_cache(&root, Some(self.fixture.cache.path())).unwrap()
    }

    /// Searches from the worktree as a daemon-backed query that does not wait.
    fn search(&self, words: &[&str]) -> String {
        search_without_waiting(&self.fixture, &self.worktree, words)
    }

    fn search_json(&self, words: &[&str]) -> Value {
        serde_json::from_str(&self.search(words)).unwrap()
    }

    fn show(&self, words: &[&str]) -> anyhow::Result<String> {
        self.fixture.run_at(&self.worktree, words)
    }
}

fn search_without_waiting(fixture: &Fixture, root: &Path, words: &[&str]) -> String {
    let mut args: Vec<String> = [
        "--workspace",
        fixture.config.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
        "--cache",
        fixture.cache.path().to_str().unwrap(),
        "--no-daemon",
        "--diagnostics",
        "off",
    ]
    .map(str::to_owned)
    .into();
    args.extend(words.iter().map(|word| (*word).to_owned()));
    let options = cli::parse(&args).unwrap();
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let cache = cache_path(&config, Some(fixture.cache.path())).unwrap();
    retrieval::search_with_policy(
        &config,
        &cache,
        &WorkspaceResults::open(&cache).unwrap(),
        &options,
        &cli::request_context(&options),
        &mut crate::semantic::SemanticSession::default(),
        QueryDeadline::start(),
        HomeIndexPolicy::AwaitDaemon(Duration::ZERO),
    )
    .unwrap()
}

fn coverage_line(text: &str) -> &str {
    text.lines()
        .find_map(|line| line.strip_prefix("coverage: "))
        .unwrap_or_else(|| panic!("no coverage line in {text}"))
}

#[test]
fn warming_worktree_answers_from_parent_index_with_exact_footer() {
    let seeded = SeededWorktree::new();
    let text = seeded.search(&["--format", "lines", "search", "SharedThing"]);
    assert_eq!(
        coverage_line(&text),
        "engine@wt-l1 warming → served from engine index (2 files differ); scope home (ws:all adds 2 members)"
    );
    let hit = text.lines().next().unwrap();
    assert!(hit.contains("\tengine/lib.rs:1-1"), "{text}");
    assert!(hit.ends_with("\tdiffers"), "{text}");
    let page = seeded.search_json(&["search", "SharedThing"]);
    let hits = page["hits"].as_array().unwrap();
    assert!(
        hits.iter().all(|hit| hit["member"] == "engine"),
        "no sibling-member hits: {page}"
    );
    assert_eq!(hits[0]["differs"], true);
    let coverage = &page["coverage"][0];
    assert_eq!(coverage["state"], "parent_fallback");
    assert_eq!(coverage["home_state"], "warming");
    assert_eq!(coverage["served_from"], "engine index");
    assert_eq!(coverage["differs"], 2);
    assert_eq!(coverage["differing_hits"], 1);
    // `sym:` re-extracts the changed file, so its hit is current and unflagged.
    let current = seeded.search(&["--format", "lines", "search", "sym:SharedThing"]);
    let hit = current.lines().next().unwrap();
    assert!(hit.contains("\tengine/lib.rs:3-3"), "{current}");
    assert!(!hit.ends_with("\tdiffers"), "{current}");
    let unchanged = seeded.search_json(&["search", "sym:untouched_helper"]);
    assert!(unchanged["hits"][0].get("differs").is_none(), "{unchanged}");
    let status: Value = serde_json::from_str(&seeded.show(&["ws", "status"]).unwrap()).unwrap();
    let engine = status["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["member"] == "engine")
        .unwrap();
    assert_eq!(engine["seed"]["outcome"], "seeded", "{status}");
}

#[test]
fn parent_answer_reads_worktree_bytes_and_worktree_only_symbols() {
    let seeded = SeededWorktree::new();
    let live = seeded.search_json(&["search", "re:worktree_marker"]);
    assert_eq!(live["hits"][0]["member"], "engine", "{live}");
    let only = seeded.search_json(&["search", "sym:WorktreeOnlySymbol"]);
    let hits = only["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{only}");
    assert!(hits[0]["file"].as_str().unwrap().ends_with("/only.rs"));
    let shown: Value =
        serde_json::from_str(&seeded.show(&["show", "sym:WorktreeOnlySymbol"]).unwrap()).unwrap();
    assert_eq!(
        shown["served_from"],
        "engine index; re-extracted in worktree"
    );
    assert_eq!(shown["lines"][0]["text"], "pub struct WorktreeOnlySymbol;");
    let map = seeded.search(&["--format", "lines", "map", "lib.rs"]);
    let added = map.lines().find(|line| line.contains("added_in_worktree"));
    assert!(
        added.is_some_and(|line| !line.ends_with("\tdiffers")),
        "{map}"
    );
    let live = seeded.search(&["--format", "lines", "search", "re:worktree_marker"]);
    assert!(!live.contains("\tdiffers"), "{live}");
    // A worktree-only redefinition joins the parent's definition of the name.
    fs::write(
        seeded.worktree.join("only.rs"),
        "pub struct WorktreeOnlySymbol;\npub fn untouched_helper() {}\n",
    )
    .unwrap();
    let both = seeded.search_json(&["search", "sym:untouched_helper"]);
    let files: Vec<_> = both["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| {
            hit["file"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(files, ["only.rs", "other.rs"], "{both}");
}

#[test]
fn show_symbol_in_changed_file_returns_worktree_span() {
    let seeded = SeededWorktree::new();
    let text = seeded
        .show(&["--format", "lines", "show", "sym:SharedThing"])
        .unwrap();
    assert!(
        text.starts_with(
            "lib.rs (engine) lines 3-3\n3\tpub struct SharedThing { pub worktree_marker: u8 }\n"
        ),
        "{text}"
    );
    assert!(text.contains("verified: true\n"), "{text}");
    assert!(
        text.contains("served_from: engine index; re-extracted in worktree\n"),
        "{text}"
    );
    let unchanged: Value =
        serde_json::from_str(&seeded.show(&["show", "sym:untouched_helper"]).unwrap()).unwrap();
    assert_eq!(unchanged["served_from"], "engine index");
    assert_eq!(unchanged["verified"], true);
}

#[test]
fn parent_view_handles_survive_worktree_publication() {
    let seeded = SeededWorktree::new();
    let handle = |query: &str| -> String {
        seeded.search_json(&["search", query])["hits"][0]["handle"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let changed = handle("SharedThing");
    let unchanged = handle("sym:untouched_helper");
    Store::open(&seeded.worktree, &seeded.worktree_cache())
        .unwrap()
        .index()
        .unwrap();
    let own = seeded.search_json(&["search", "sym:SharedThing"]);
    assert_eq!(own["coverage"][0]["state"], "searched", "{own}");
    let shown: Value = serde_json::from_str(&seeded.show(&["show", &unchanged]).unwrap()).unwrap();
    assert_eq!(shown["served_from"], "engine index");
    assert_eq!(shown["lines"][0]["text"], "pub fn untouched_helper() {}");
    let shown: Value = serde_json::from_str(&seeded.show(&["show", &changed]).unwrap()).unwrap();
    assert_eq!(
        shown["served_from"],
        "engine index; re-extracted in worktree"
    );
    let text = shown["lines"][0]["text"].as_str().unwrap();
    assert!(text.contains("worktree_marker"), "{shown}");
}

#[test]
fn warming_or_unavailable_home_never_widens() {
    let fixture = Fixture::git_backed();
    fixture.json("engine", &["search", "sym:SharedThing", "in:pack"]);
    fixture.json("engine", &["search", "sym:SharedThing", "in:upstream"]);
    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("wt-l2"));
    let text = search_without_waiting(
        &fixture,
        &worktree,
        &["--format", "lines", "search", "sym:SharedThing"],
    );
    assert_eq!(
        text,
        "coverage: engine@wt-l2 warming (no parent index); scope home (warming; ws:all searches 2 members)\n"
    );
    let seeded = SeededWorktree::new();
    fs::write(
        seeded.worktree_cache().join("index.sqlite3"),
        "not a database",
    )
    .unwrap();
    let page = seeded.search_json(&["search", "sym:SharedThing"]);
    assert!(page["hits"].as_array().unwrap().is_empty(), "{page}");
    assert_eq!(
        page["scope"],
        "home (unavailable; ws:all searches 2 members)"
    );
    let reason = page["coverage"][0]["reason"].as_str().unwrap();
    assert!(
        !reason.is_empty() && reason.chars().count() <= 121,
        "{reason}"
    );
    let lines = seeded.search(&["--format", "lines", "search", "sym:SharedThing"]);
    assert!(
        coverage_line(&lines).starts_with(&format!("engine@wt-l1 unavailable ({reason}")),
        "{lines}"
    );
}

#[test]
fn divergence_covers_edited_untracked_parent_advanced_and_parent_dirty() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent");
    fs::create_dir(&parent).unwrap();
    for name in ["edited.rs", "same.rs", "dirty.rs"] {
        fs::write(parent.join(name), format!("// {name}\n")).unwrap();
    }
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["add", "."]);
    git(&parent, &["commit", "-q", "-m", "init"]);
    let worktree = directory.path().join("wt");
    let worktree_arg = worktree.to_str().unwrap();
    git(
        &parent,
        &["worktree", "add", "-q", "--detach", worktree_arg],
    );
    fs::write(worktree.join("edited.rs"), "// changed\n").unwrap();
    fs::write(worktree.join("untracked.rs"), "// new\n").unwrap();
    fs::write(parent.join("advanced.rs"), "// parent only\n").unwrap();
    git(&parent, &["add", "advanced.rs"]);
    git(&parent, &["commit", "-q", "-m", "advance"]);
    fs::write(parent.join("dirty.rs"), "// uncommitted\n").unwrap();
    let cache = directory.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let divergence = WorktreeDivergence::load_or_compute(&parent, &worktree, &cache, 1);
    assert!(divergence.complete);
    assert_eq!(
        divergence
            .paths
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["advanced.rs", "dirty.rs", "edited.rs", "untracked.rs"]
    );
    assert!(cache.join("divergence.json").is_file());
    let reused = WorktreeDivergence::load_or_compute(&parent, &worktree, &cache, 1);
    assert_eq!(reused, divergence);
    let not_git = directory.path().join("plain");
    fs::create_dir(&not_git).unwrap();
    assert!(!WorktreeDivergence::load_or_compute(&parent, &not_git, &cache, 1).complete);
}

#[test]
fn parent_view_requires_a_linked_worktree_of_the_member() {
    let seeded = SeededWorktree::new();
    let config = WorkspaceConfig::load(&seeded.fixture.config).unwrap();
    let engine = config.members.iter().find(|m| m.name == "engine").unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    git(unrelated.path(), &["init", "-q", "-b", "main"]);
    let mut impostor =
        MemberRoot::linked(engine, unrelated.path().canonicalize().unwrap()).unwrap();
    impostor.is_home = true;
    let cache = member_cache(&impostor, Some(seeded.fixture.cache.path())).unwrap();
    let resolved = super::resolve_home_index(
        &impostor,
        &cache,
        Some(seeded.fixture.cache.path()),
        HomeIndexPolicy::AwaitDaemon(Duration::ZERO),
        QueryDeadline::start(),
    );
    assert!(
        matches!(resolved, super::HomeIndexSource::Warming { .. }),
        "an unrelated checkout must not read through the member's index"
    );
}

#[test]
fn unextractable_worktree_file_falls_back_to_unverified_parent_bytes() {
    let seeded = SeededWorktree::new();
    let handle = seeded.search_json(&["search", "SharedThing"])["hits"][0]["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::write(
        seeded.worktree.join("lib.rs"),
        b"pub struct SharedThing;\0binary",
    )
    .unwrap();
    for target in [handle.as_str(), "sym:SharedThing"] {
        let shown: Value = serde_json::from_str(&seeded.show(&["show", target]).unwrap()).unwrap();
        assert_eq!(shown["verified"], false, "{shown}");
        assert_eq!(shown["source"], "parent_index");
        assert_eq!(
            shown["served_from"],
            "engine index; worktree file not extractable"
        );
        let text = shown["lines"][0]["text"].as_str().unwrap();
        assert!(text.contains("engine_marker"), "{shown}");
    }
}

#[test]
fn hash_check_catches_changes_git_divergence_misses() {
    let fixture = Fixture::git_backed();
    fixture.json("engine", &["search", "sym:SharedThing", "ws:all"]);
    // The parent index lags its HEAD: lines move in a commit it has not indexed.
    let engine = fixture.root.path().join("engine");
    fs::write(
        engine.join("lib.rs"),
        "// moved\n\n\npub struct SharedThing { pub engine_marker: u8 }\n",
    )
    .unwrap();
    git(&engine, &["commit", "-q", "-am", "move"]);
    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("wt-l3"));
    let text = search_without_waiting(
        &fixture,
        &worktree,
        &["--format", "lines", "search", "SharedThing"],
    );
    assert!(
        coverage_line(&text)
            .starts_with("engine@wt-l3 warming → served from engine index (1 file differs)"),
        "{text}"
    );
    assert!(
        text.lines().next().unwrap().ends_with("\tdiffers"),
        "{text}"
    );
    let symbol = search_without_waiting(
        &fixture,
        &worktree,
        &["--format", "lines", "search", "sym:SharedThing"],
    );
    assert!(symbol.contains("\tengine/lib.rs:4-4"), "{symbol}");
    let shown: Value = serde_json::from_str(
        &fixture
            .run_at(&worktree, &["show", "sym:SharedThing"])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(shown["lines"][0]["line"], 4, "{shown}");
    // An unstaged edit inside the divergence reuse window is caught by hash too.
    fs::write(worktree.join("lib.rs"), "\npub struct SharedThing;\n").unwrap();
    let edited = search_without_waiting(&fixture, &worktree, &["search", "sym:SharedThing"]);
    let edited: Value = serde_json::from_str(&edited).unwrap();
    assert_eq!(edited["hits"][0]["start_line"], 2, "{edited}");
}
