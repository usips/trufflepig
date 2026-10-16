//! Standalone linked-worktree reads keep parent and child result identities separate.

mod cache_progress_tests;

use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde_json::Value;
use std::{
    fs,
    fs::OpenOptions,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const CHILD_CASE: &str = "TRUFFLEPIG_STANDALONE_PARENT_CASE";

#[test]
fn cold_parent_fallback_reads_current_worktree_without_child_publication() -> Result<()> {
    if !in_isolated_child("cold_parent_fallback_reads_current_worktree_without_child_publication")?
    {
        return Ok(());
    }

    let fixture = WorktreeFixture::new()?;
    fixture.assert_child_unpublished();

    let search = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["search", "sym:WorktreeOnlySymbol"],
        &[],
    )?)?;
    assert_eq!(
        search["coverage"]["parent_index"]["generation"],
        fixture.parent_generation
    );
    assert_eq!(
        search["coverage"]["parent_index"]["root"],
        crate::store::encode_path(&fixture.main)
    );
    assert_eq!(search["hits"].as_array().unwrap().len(), 2, "{search}");
    let handle = search["hits"][0]["handle"]
        .as_str()
        .context("search hit has no handle")?;
    let shown = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["show", handle],
        &[],
    )?)?;
    assert!(
        shown["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line["text"]
                .as_str()
                .unwrap_or_default()
                .contains("WorktreeOnlySymbol")),
        "show must read current worktree bytes: {shown}"
    );

    let map = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["map", "src/lib.rs"],
        &[],
    )?)?;
    assert_eq!(
        map["coverage"]["parent_index"]["generation"],
        fixture.parent_generation
    );
    assert!(
        map["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["name"] == "worktree_only_helper"),
        "map must include the changed worktree definition: {map}"
    );

    let new_file_map = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["map", "src/only.rs"],
        &[],
    )?)?;
    assert_eq!(
        new_file_map["coverage"]["parent_index"]["generation"],
        fixture.parent_generation
    );
    assert!(
        new_file_map["coverage"].get("no_indexed_path").is_none(),
        "mapping a newly added worktree file clears the parent-index miss: {new_file_map}"
    );
    assert!(
        new_file_map["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["name"] == "WorktreeOnlySymbol"),
        "map must extract the new worktree file outline: {new_file_map}"
    );

    let path_read = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["show", "path:src/long.txt:1-240"],
        &["--budget".into(), "350".into()],
    )?)?;
    assert_eq!(path_read["source"], "current_file", "{path_read}");
    assert_eq!(path_read["verified"], false, "{path_read}");
    let cursor = path_read["next"]
        .as_str()
        .context("long current-file read must return a continuation")?;
    assert!(cursor.starts_with("read:path:"), "{cursor}");
    let (cursor_span, cursor_revision, cursor_offset) = stateless_cursor_parts(cursor)?;
    assert_eq!(
        cursor_span,
        path_cursor_span(&fixture.worktree, &path_read["start"], &path_read["end"])?
    );
    assert_eq!(cursor_revision, path_read["revision"].as_str().unwrap());
    assert!(cursor_offset > path_read["start"].as_u64().unwrap() as usize);
    assert_eq!(
        path_read["lines"].as_array().unwrap().last().unwrap()["end"],
        cursor_offset,
        "cursor must start at the first unread byte: {path_read}"
    );

    let continued = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["show", cursor],
        &[],
    )?)?;
    assert_eq!(continued["verified"], false, "{continued}");
    assert_eq!(continued["source"], "current_file", "{continued}");
    assert_eq!(continued["revision"], path_read["revision"], "{continued}");
    assert_eq!(continued["start"], cursor_offset, "{continued}");
    assert_eq!(continued["end"], path_read["end"], "{continued}");
    assert!(
        continued["lines"]
            .as_array()
            .unwrap()
            .first()
            .is_some_and(|line| {
                line["start"] == cursor_offset
                    && line["text"].as_str().unwrap_or_default().contains(&format!(
                        "child_current_line_{:03}",
                        path_read["lines"].as_array().unwrap().len() + 1
                    ))
            }),
        "continuation must advance to the next current worktree line: {continued}"
    );
    if let Some(next) = continued["next"].as_str() {
        let (next_span, next_revision, next_offset) = stateless_cursor_parts(next)?;
        assert_eq!(next_span, cursor_span);
        assert_eq!(next_revision, cursor_revision);
        assert_eq!(
            continued["lines"].as_array().unwrap().last().unwrap()["end"],
            next_offset,
            "next cursor must start at the first unread byte: {continued}"
        );
        assert!(next_offset > cursor_offset, "{continued}");
    }
    fixture.assert_child_unpublished();
    assert_eq!(
        fixture.current_parent_generation()?,
        fixture.parent_generation
    );
    Ok(())
}

#[test]
fn saved_parent_handle_and_more_keep_parent_origin_after_child_publication() -> Result<()> {
    if !in_isolated_child(
        "saved_parent_handle_and_more_keep_parent_origin_after_child_publication",
    )? {
        return Ok(());
    }

    let fixture = WorktreeFixture::new()?;
    let first = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["search", "sym:WorktreeOnlySymbol"],
        &["--limit".into(), "1".into()],
    )?)?;
    assert_eq!(
        first["coverage"]["parent_index"]["generation"],
        fixture.parent_generation
    );
    let handle = first["hits"][0]["handle"]
        .as_str()
        .context("fallback hit has no handle")?
        .to_owned();
    let cursor = first["next"]
        .as_str()
        .context("multiple current worktree hits should paginate")?
        .to_owned();
    assert_eq!(
        saved_generation(&fixture.worktree, &fixture.child_cache, &handle)?,
        fixture.parent_generation
    );
    fixture.assert_child_unpublished();

    let current = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["show", "path:src/long.txt:1-240"],
        &["--budget".into(), "350".into()],
    )?)?;
    assert_eq!(current["verified"], false, "{current}");
    assert_eq!(current["source"], "current_file", "{current}");
    let current_cursor = current["next"]
        .as_str()
        .context("expected path continuation")?;
    assert!(current_cursor.starts_with("read:path:"), "{current_cursor}");
    let (cursor_span, cursor_revision, cursor_offset) = stateless_cursor_parts(current_cursor)?;
    assert_eq!(
        cursor_span,
        path_cursor_span(&fixture.worktree, &current["start"], &current["end"])?
    );
    assert_eq!(cursor_revision, current["revision"].as_str().unwrap());
    assert!(cursor_offset > current["start"].as_u64().unwrap() as usize);
    assert_eq!(
        current["lines"].as_array().unwrap().last().unwrap()["end"],
        cursor_offset,
        "cursor must start at the first unread byte: {current}"
    );

    let mut child = crate::store::Store::open(&fixture.worktree, &fixture.child_cache)?;
    child.index()?;
    assert_eq!(child.generation()?, 1);
    drop(child);
    assert_eq!(
        fixture.current_parent_generation()?,
        fixture.parent_generation
    );

    // A generation-zero source read stays valid after a child publication.
    let continued = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["show", current_cursor],
        &[],
    )?)?;
    assert_eq!(continued["verified"], false, "{continued}");
    assert_eq!(continued["source"], "current_file", "{continued}");
    assert_eq!(continued["revision"], current["revision"], "{continued}");
    assert_eq!(continued["start"], cursor_offset, "{continued}");
    assert_eq!(continued["end"], current["end"], "{continued}");
    assert!(
        continued["lines"]
            .as_array()
            .unwrap()
            .first()
            .is_some_and(|line| {
                line["start"] == cursor_offset
                    && line["text"].as_str().unwrap_or_default().contains(&format!(
                        "child_current_line_{:03}",
                        current["lines"].as_array().unwrap().len() + 1
                    ))
            }),
        "continuation must advance to the next current worktree line: {continued}"
    );
    if let Some(next) = continued["next"].as_str() {
        let (next_span, next_revision, next_offset) = stateless_cursor_parts(next)?;
        assert_eq!(next_span, cursor_span);
        assert_eq!(next_revision, cursor_revision);
        assert_eq!(
            continued["lines"].as_array().unwrap().last().unwrap()["end"],
            next_offset,
            "next cursor must start at the first unread byte: {continued}"
        );
        assert!(next_offset > cursor_offset, "{continued}");
    }

    let next = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["more", &cursor],
        &["--limit".into(), "1".into()],
    )?)?;
    assert_eq!(next["generation"], fixture.parent_generation, "{next}");
    assert_eq!(
        next["coverage"]["parent_index"]["generation"], fixture.parent_generation,
        "more must page the saved parent result set: {next}"
    );
    assert_eq!(next["hits"].as_array().unwrap().len(), 1, "{next}");

    let error = invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["ctx", &handle],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("stale_result"),
        "a saved parent hit whose current source differs must not be resolved against child graph IDs: {error}"
    );
    assert_eq!(
        fixture.current_parent_generation()?,
        fixture.parent_generation
    );

    fs::write(
        fixture.main.join("src/new_parent.rs"),
        "pub fn parent_added() {}\n",
    )?;
    let mut parent = crate::store::Store::open(&fixture.main, &fixture.main_cache)?;
    parent.index()?;
    assert!(parent.generation()? > fixture.parent_generation);
    drop(parent);

    let next = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["more", &cursor],
        &[],
    )?)?;
    assert_eq!(next["generation"], fixture.parent_generation, "{next}");
    let error = invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["show", &handle],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("stale_result: parent index changed"),
        "{error}"
    );

    // Saved pages and path cursors also work when neither index can be opened.
    let child_database = fixture.child_cache.join("index.sqlite3");
    fs::set_permissions(&child_database, fs::Permissions::from_mode(0o000))?;
    let error = crate::store::Store::open_read(
        &fixture.worktree,
        &fixture.child_cache,
        crate::daemon::deadline::QueryDeadline::start(),
    )
    .err()
    .context("fixture must deny opening the index")?;
    assert!(crate::store::is_index_cannot_open(&error), "{error:#}");
    for words in [
        vec!["search", "sym:WorktreeOnlySymbol"],
        vec!["map", "src/only.rs"],
    ] {
        let fallback = json(invoke(
            &fixture.worktree,
            &fixture.child_cache,
            &words,
            &[],
        )?)?;
        assert_eq!(
            fallback["coverage"]["state"], "parent_fallback",
            "{fallback}"
        );
        assert!(
            !fallback["hits"].as_array().unwrap().is_empty(),
            "{fallback}"
        );
    }
    fs::remove_file(fixture.main_cache.join("index.sqlite3"))?;
    let next = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["more", &cursor],
        &[],
    )?)?;
    assert_eq!(next["generation"], fixture.parent_generation, "{next}");
    let continued = json(invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["show", current_cursor],
        &[],
    )?)?;
    assert_eq!(continued["verified"], false, "{continued}");
    assert_eq!(continued["source"], "current_file", "{continued}");
    assert_eq!(continued["revision"], current["revision"], "{continued}");
    assert_eq!(continued["start"], cursor_offset, "{continued}");
    assert_eq!(continued["end"], current["end"], "{continued}");
    assert!(
        continued["lines"]
            .as_array()
            .unwrap()
            .first()
            .is_some_and(|line| {
                line["start"] == cursor_offset
                    && line["text"].as_str().unwrap_or_default().contains(&format!(
                        "child_current_line_{:03}",
                        current["lines"].as_array().unwrap().len() + 1
                    ))
            }),
        "continuation must advance to the next current worktree line: {continued}"
    );
    if let Some(next) = continued["next"].as_str() {
        let (next_span, next_revision, next_offset) = stateless_cursor_parts(next)?;
        assert_eq!(next_span, cursor_span);
        assert_eq!(next_revision, cursor_revision);
        assert_eq!(
            continued["lines"].as_array().unwrap().last().unwrap()["end"],
            next_offset,
            "next cursor must start at the first unread byte: {continued}"
        );
        assert!(next_offset > cursor_offset, "{continued}");
    }
    fs::set_permissions(&child_database, fs::Permissions::from_mode(0o600))?;

    let moved = fixture.scratch.path().join("replaced-worktree");
    fs::rename(&fixture.worktree, &moved)?;
    fs::create_dir(&fixture.worktree)?;
    fs::copy(moved.join(".git"), fixture.worktree.join(".git"))?;
    let error = invoke(
        &fixture.worktree,
        &fixture.child_cache,
        &["more", &cursor],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("stale_result: linked worktree was replaced"),
        "{error}"
    );
    Ok(())
}

#[test]
fn no_daemon_read_serves_parent_hit_promptly_while_child_writer_is_held() -> Result<()> {
    if !in_isolated_child("no_daemon_read_serves_parent_hit_promptly_while_child_writer_is_held")? {
        return Ok(());
    }

    let fixture = WorktreeFixture::new()?;
    fs::create_dir_all(&fixture.child_cache)?;
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(fixture.child_cache.join("index.lock"))?;
    lease.lock_exclusive()?;
    let options = crate::cli::parse(&[
        "--root".into(),
        fixture.worktree.display().to_string(),
        "--cache".into(),
        fixture.child_cache.display().to_string(),
        "--no-daemon".into(),
        "--format".into(),
        "json".into(),
        "--diagnostics".into(),
        "off".into(),
        "search".into(),
        "sym:WorktreeOnlySymbol".into(),
    ])?;

    let started = Instant::now();
    let output = crate::cli::local(&fixture.worktree, &fixture.child_cache, &options, false)?;
    assert!(started.elapsed() < Duration::from_secs(1), "{output}");
    let response = json(output)?;
    assert_eq!(
        response["coverage"]["parent_index"]["generation"], fixture.parent_generation,
        "{response}"
    );
    assert!(
        response["hits"]
            .as_array()
            .is_some_and(|hits| hits.iter().any(|hit| hit["name"] == "WorktreeOnlySymbol")),
        "{response}"
    );
    assert_unpublished_index(&fixture.worktree, &fixture.child_cache)?;
    drop(lease);
    Ok(())
}

#[test]
fn explicit_cache_reads_through_parent_and_keeps_selected_result_identity() -> Result<()> {
    if !in_isolated_child("explicit_cache_reads_through_parent_and_keeps_selected_result_identity")?
    {
        return Ok(());
    }

    let fixture = WorktreeFixture::new()?;
    let custom_cache = fixture.scratch.path().join("custom-worktree-cache");
    fs::create_dir_all(&custom_cache)?;
    let explicit_cache = [
        "--cache".to_owned(),
        custom_cache.display().to_string(),
        "--no-daemon".to_owned(),
    ];
    let search_options = [
        explicit_cache[0].clone(),
        explicit_cache[1].clone(),
        explicit_cache[2].clone(),
        "--limit".to_owned(),
        "1".to_owned(),
    ];
    let search = json(invoke(
        &fixture.worktree,
        &custom_cache,
        &["search", "sym:WorktreeOnlySymbol"],
        &search_options,
    )?)?;
    assert!(
        search["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty()),
        "explicit-cache search reads through the verified parent: {search}"
    );
    assert_eq!(
        search["coverage"]["parent_index"]["generation"],
        fixture.parent_generation
    );
    let handle = search["hits"][0]["handle"]
        .as_str()
        .context("parent hit has no handle")?
        .to_owned();
    let cursor = search["next"]
        .as_str()
        .context("parent search must paginate")?
        .to_owned();
    assert_eq!(
        saved_generation(&fixture.worktree, &custom_cache, &handle)?,
        fixture.parent_generation,
        "result handles use the selected cache and retain their parent generation"
    );

    let shown = json(invoke(
        &fixture.worktree,
        &custom_cache,
        &["show", &handle],
        &explicit_cache,
    )?)?;
    assert!(
        shown["lines"].as_array().unwrap().iter().any(|line| {
            line["text"]
                .as_str()
                .unwrap_or_default()
                .contains("WorktreeOnlySymbol")
        }),
        "saved parent show uses current worktree bytes: {shown}"
    );

    let next = json(invoke(
        &fixture.worktree,
        &custom_cache,
        &["more", &cursor],
        &explicit_cache,
    )?)?;
    assert_eq!(next["generation"], fixture.parent_generation, "{next}");
    assert_eq!(
        next["coverage"]["parent_index"]["generation"],
        fixture.parent_generation
    );
    let context_error = invoke(
        &fixture.worktree,
        &custom_cache,
        &["ctx", &handle],
        &explicit_cache,
    )
    .unwrap_err()
    .to_string();
    assert!(context_error.contains("stale_result"), "{context_error}");

    let map = json(invoke(
        &fixture.worktree,
        &custom_cache,
        &["map", "src/lib.rs"],
        &explicit_cache,
    )?)?;
    assert_eq!(
        map["coverage"]["parent_index"]["generation"],
        fixture.parent_generation
    );
    assert!(
        map["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["name"] == "worktree_only_helper"),
        "explicit-cache map uses the parent index with worktree bytes: {map}"
    );

    let wrong_root_cache = fixture.scratch.path().join("wrong-root-cache");
    let mut foreign = crate::store::Store::open(&fixture.main, &wrong_root_cache)?;
    foreign.index()?;
    drop(foreign);
    let wrong_root_error = invoke(
        &fixture.worktree,
        &wrong_root_cache,
        &["search", "sym:WorktreeOnlySymbol"],
        &["--cache".into(), wrong_root_cache.display().to_string()],
    )
    .unwrap_err()
    .to_string();
    assert!(
        wrong_root_error.contains("cache belongs to another repository root"),
        "a custom cache with the wrong identity must not fall back to the parent: {wrong_root_error}"
    );

    assert!(custom_cache.join("results.sqlite3").is_file());
    assert!(!custom_cache.join("index.sqlite3").exists());
    fixture.assert_child_unpublished();
    assert_eq!(
        fixture.current_parent_generation()?,
        fixture.parent_generation
    );
    Ok(())
}

struct WorktreeFixture {
    scratch: tempfile::TempDir,
    main: PathBuf,
    worktree: PathBuf,
    main_cache: PathBuf,
    child_cache: PathBuf,
    parent_generation: i64,
}

impl WorktreeFixture {
    fn new() -> Result<Self> {
        let scratch = crate::board::board_test_support::scratch("standalone-parent-");
        let main = scratch.path().join("main");
        let worktree = scratch.path().join("linked-worktree");
        fs::create_dir_all(main.join("src"))?;
        fs::write(
            main.join("src/lib.rs"),
            "pub struct SharedThing;\npub fn parent_only_helper() {}\n",
        )?;
        git(&main, &["init", "--quiet", "--initial-branch=main"])?;
        git(&main, &["config", "user.name", "Trufflepig Test"])?;
        git(
            &main,
            &["config", "user.email", "trufflepig-test@example.invalid"],
        )?;
        git(&main, &["add", "."])?;
        git(&main, &["commit", "--quiet", "-m", "initial fixture"])?;

        let main_cache = crate::cli::cache_path(&main, None)?;
        let mut parent = crate::store::Store::open(&main, &main_cache)?;
        parent.index()?;
        let parent_generation = parent.generation()?;
        drop(parent);

        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                worktree.to_str().context("fixture path is not UTF-8")?,
            ],
        )?;
        fs::write(
            worktree.join("src/lib.rs"),
            "pub struct SharedThing { pub worktree_marker: u8 }\npub fn worktree_only_helper() {}\npub struct WorktreeOnlySymbol;\n",
        )?;
        fs::write(
            worktree.join("src/only.rs"),
            "pub struct WorktreeOnlySymbol;\n",
        )?;
        let long_source = (1..=240)
            .map(|line| {
                format!(
                    "child_current_line_{line:03} alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu\n"
                )
            })
            .collect::<String>();
        fs::write(worktree.join("src/long.txt"), long_source)?;

        let child_cache = crate::cli::cache_path(&worktree, None)?;
        ensure!(
            !child_cache.join("index.sqlite3").exists(),
            "fixture child cache must start unpublished"
        );
        Ok(Self {
            scratch,
            main,
            worktree,
            main_cache,
            child_cache,
            parent_generation,
        })
    }

    fn assert_child_unpublished(&self) {
        assert!(
            !self.child_cache.join("index.sqlite3").exists(),
            "standalone reads must not publish the child index"
        );
    }

    fn current_parent_generation(&self) -> Result<i64> {
        Ok(crate::store::Store::open_read(
            &self.main,
            &self.main_cache,
            crate::daemon::deadline::QueryDeadline::start(),
        )?
        .generation()?)
    }
}

fn invoke(root: &Path, cache: &Path, words: &[&str], extra: &[String]) -> Result<String> {
    let words = words.iter().map(|word| (*word).to_owned()).collect();
    invoke_owned(root, cache, words, extra)
}

fn invoke_owned(root: &Path, cache: &Path, words: Vec<String>, extra: &[String]) -> Result<String> {
    let mut args = vec![
        "--root".into(),
        root.display().to_string(),
        "--format".into(),
        "json".into(),
        "--diagnostics".into(),
        "off".into(),
    ];
    args.extend(extra.iter().cloned());
    args.extend(words);
    let options = crate::cli::parse(&args)?;
    let resolved_cache = crate::cli::cache_path(root, options.cache.as_deref())?;
    ensure!(
        resolved_cache == cache,
        "test helper cache differs from parsed CLI cache"
    );
    crate::cli::local(root, &resolved_cache, &options, true)
}

fn json(value: String) -> Result<Value> {
    Ok(serde_json::from_str(&value)?)
}

fn stateless_cursor_parts(cursor: &str) -> Result<(&str, &str, usize)> {
    let body = cursor
        .strip_prefix("read:")
        .context("stateless continuation must start with read:")?;
    let (span_revision, offset) = body
        .rsplit_once('@')
        .context("stateless continuation has no byte offset")?;
    let (span, revision) = span_revision
        .rsplit_once('@')
        .context("stateless continuation has no source revision")?;
    Ok((span, revision, offset.parse()?))
}

fn path_cursor_span(root: &Path, start: &Value, end: &Value) -> Result<String> {
    let canonical_root = root.canonicalize()?;
    let root_digest = blake3::hash(canonical_root.as_os_str().as_bytes()).to_hex();
    Ok(format!(
        "path:{root_digest}:src/long.txt:{}-{}",
        start.as_u64().context("path read has no start byte")?,
        end.as_u64().context("path read has no end byte")?,
    ))
}

fn saved_generation(root: &Path, cache: &Path, handle: &str) -> Result<i64> {
    let parsed: crate::identity::ResultHandle = handle.parse()?;
    let store = crate::store::Store::unpublished(root, cache)?;
    Ok(crate::results::load_entries(&store, &parsed.set.simple().to_string())?.generation)
}

fn assert_unpublished_index(root: &Path, cache: &Path) -> Result<()> {
    let index = cache.join("index.sqlite3");
    ensure!(
        index.is_file(),
        "reconcile attempt should initialize the index schema"
    );
    let connection =
        rusqlite::Connection::open_with_flags(&index, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let generation: String =
        connection.query_row("SELECT value FROM meta WHERE key='generation'", [], |row| {
            row.get(0)
        })?;
    ensure!(
        generation == "0",
        "held lease published generation {generation}"
    );
    let publication_count: i64 =
        connection.query_row("SELECT count(*) FROM publication_windows", [], |row| {
            row.get(0)
        })?;
    ensure!(
        publication_count == 0,
        "held lease wrote publication records"
    );

    let error = match crate::store::Store::open_read(
        root,
        cache,
        crate::daemon::deadline::QueryDeadline::start(),
    ) {
        Ok(_) => anyhow::bail!("generation-zero index must be warming"),
        Err(error) => error,
    };
    ensure!(crate::store::is_index_warming(&error), "{error:#}");
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    ensure!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn in_isolated_child(case: &str) -> Result<bool> {
    if std::env::var(CHILD_CASE).as_deref() == Ok(case) {
        return Ok(true);
    }

    let directory = crate::board::board_test_support::scratch("standalone-parent-process-");
    let paths = [
        "home", "cache", "config", "data", "runtime", "system", "spool", "tmp",
    ]
    .map(|name| directory.path().join(name));
    for path in &paths {
        fs::create_dir_all(path)?;
    }
    fs::set_permissions(&paths[4], fs::Permissions::from_mode(0o700))?;

    let test_name = format!("cli::tests::standalone_parent_tests::{case}");
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            test_name.as_str(),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_CASE, case)
        .env("HOME", &paths[0])
        .env("XDG_CACHE_HOME", &paths[1])
        .env("XDG_CONFIG_HOME", &paths[2])
        .env("XDG_DATA_HOME", &paths[3])
        .env("XDG_RUNTIME_DIR", &paths[4])
        .env("TRUFFLEPIG_SYSTEM_DIR", &paths[5])
        .env("TRUFFLEPIG_SPOOL_DIR", &paths[6])
        .env("TMPDIR", &paths[7])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let timed_out = loop {
        if child.try_wait()?.is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            break true;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}{stderr}");
    ensure!(!timed_out, "standalone parent fixture exceeded 90 seconds");
    ensure!(
        output.status.success(),
        "standalone parent fixture failed: {stdout}{stderr}"
    );
    ensure!(
        stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed; 0 ignored"),
        "child must execute the requested test: {stdout}"
    );
    Ok(false)
}
