#![cfg(unix)]

mod current_path_cli {
    pub(super) mod busy_daemon;
    pub(super) mod process_fixture;
}

use current_path_cli::{busy_daemon::BusyDaemon, process_fixture::Fixture};
use serde_json::Value;
use std::{
    fs,
    time::{Duration, Instant},
};

const BUSY_READ_ATTEMPTS: usize = 5;

#[test]
fn no_daemon_reads_current_markdown_without_writing_the_cache() {
    let fixture = Fixture::new();
    let source = fixture.write_markdown(240);
    let _readonly = fixture.make_cache_readonly();
    let cache_before = fixture.cache_snapshot(&fixture.cache);
    let default_cache_before = fixture.default_cache_snapshot();

    let whole = fixture.run_show(
        "path:docs/guide.md",
        &fixture.cache,
        true,
        Duration::from_secs(10),
    );
    assert_current_path(&whole, "docs/guide.md");
    assert_eq!(whole["start"], 0);
    assert_eq!(whole["end"], source.len());
    assert_eq!(whole["lines"].as_array().unwrap().len(), 200);
    assert_eq!(whole["truncated"], true);
    assert!(whole["next"].as_str().is_some());

    let ranged = fixture.run_show(
        "path:docs/guide.md:10-235",
        &fixture.cache,
        true,
        Duration::from_secs(10),
    );
    assert_current_path(&ranged, "docs/guide.md");
    assert_eq!(ranged["lines"].as_array().unwrap().len(), 200);
    assert_eq!(ranged["lines"][0]["line"], 10);
    assert_eq!(ranged["truncated"], true);
    assert!(ranged["next"].as_str().is_some());

    let short_range = fixture.run_show(
        "path:docs/guide.md:10-12",
        &fixture.cache,
        true,
        Duration::from_secs(10),
    );
    assert_current_path(&short_range, "docs/guide.md");
    assert_eq!(short_range["lines"].as_array().unwrap().len(), 3);
    assert_eq!(short_range["truncated"], false);
    assert!(short_range["next"].is_null());

    assert_eq!(fixture.cache_snapshot(&fixture.cache), cache_before);
    assert_eq!(fixture.default_cache_snapshot(), default_cache_before);
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
    assert!(_readonly.mode_is_read_only());

    let blocked_cache = fixture.make_regular_cache_file();
    let blocked_before = fs::read(&blocked_cache).unwrap();
    let read = fixture.run_show(
        "path:docs/guide.md:10-12",
        &blocked_cache,
        true,
        Duration::from_secs(10),
    );
    assert_current_path(&read, "docs/guide.md");
    assert_eq!(fs::read(&blocked_cache).unwrap(), blocked_before);
}

#[test]
fn isolated_router_reads_current_path_with_a_blocked_cache_file() {
    let fixture = Fixture::new();
    fixture.write_markdown(240);
    let blocked_cache = fixture.make_regular_cache_file();
    let blocked_before = fixture.cache_snapshot(&blocked_cache);
    let mut router = fixture.start_system_server(&blocked_cache, Duration::from_secs(15));
    router.assert_running();
    let default_cache_before = fixture.default_cache_snapshot();

    let read = fixture.run_show(
        "path:docs/guide.md",
        &blocked_cache,
        false,
        Duration::from_secs(25),
    );
    assert_current_path(&read, "docs/guide.md");
    assert_eq!(read["lines"].as_array().unwrap().len(), 200);
    assert_eq!(read["truncated"], true);
    router.assert_running();
    let cursor = read["next"]
        .as_str()
        .expect("router path read is truncated");
    assert!(cursor.starts_with("read:path:"), "{cursor}");

    let continued = fixture.run_show(cursor, &blocked_cache, false, Duration::from_secs(25));
    assert_current_path(&continued, "docs/guide.md");
    assert_eq!(continued["lines"].as_array().unwrap().len(), 40);
    assert_eq!(continued["lines"][0]["line"], 201);
    assert_eq!(continued["lines"][39]["line"], 240);
    router.assert_running();

    let ranged = fixture.run_show(
        "path:docs/guide.md:10-12",
        &blocked_cache,
        false,
        Duration::from_secs(25),
    );
    assert_current_path(&ranged, "docs/guide.md");
    assert_eq!(ranged["lines"].as_array().unwrap().len(), 3);
    assert_eq!(ranged["truncated"], false);
    router.assert_running();

    let (workspace, selected_root) = fixture.write_workspace();
    let workspace_read = fixture.run_workspace_show(
        &workspace,
        "path:docs/guide.md",
        &blocked_cache,
        false,
        Duration::from_secs(25),
    );
    assert_selected_workspace_read(&workspace_read, &selected_root);
    router.assert_running();
    assert_eq!(fixture.cache_snapshot(&blocked_cache), blocked_before);
    assert_eq!(fixture.default_cache_snapshot(), default_cache_before);
}

#[test]
fn cold_process_held_busy_fallback_returns_current_path_within_shared_deadline() {
    let fixture = Fixture::new();
    let source = fixture.write_markdown_with_prefix("COLD_BUSY_FALLBACK_ACCEPTED", 12);
    let _readonly = fixture.make_cache_readonly();
    let cache_before = fixture.cache_snapshot(&fixture.cache);
    let default_cache_before = fixture.default_cache_snapshot();
    let mut busy = BusyDaemon::start_busy_then_hold(&fixture.system_dir);

    let started = Instant::now();
    let read = fixture.run_show(
        "path:docs/guide.md",
        &fixture.cache,
        false,
        Duration::from_secs(35),
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed <= Duration::from_secs(32),
        "cold fallback exceeded the 30-second client budget allowance: {elapsed:?}"
    );
    assert_current_path(&read, "docs/guide.md");
    assert_eq!(read["end"], source.len());
    assert_eq!(read["lines"].as_array().unwrap().len(), 12);
    assert_eq!(
        read["lines"][0]["text"],
        "# COLD_BUSY_FALLBACK_ACCEPTED row-001\n"
    );
    assert_request_attempts(&busy, "path:docs/guide.md", 2);
    assert!(busy.all_request_ids_are_fresh());
    assert!(busy.requests_preserve_root_and_cache(&fixture.root, &fixture.cache));
    assert_eq!(fixture.cache_snapshot(&fixture.cache), cache_before);
    assert_eq!(fixture.default_cache_snapshot(), default_cache_before);
    busy.stop();
}

#[test]
fn silent_router_current_path_read_returns_local_bytes_within_budget() {
    let fixture = Fixture::new();
    let source = fixture.write_markdown_with_prefix("SILENT_ROUTER_ACCEPTED", 12);
    let mut silent = BusyDaemon::start_silent(&fixture.system_dir);

    let started = Instant::now();
    let read = fixture.run_show(
        "path:docs/guide.md",
        &fixture.cache,
        false,
        Duration::from_secs(35),
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed <= Duration::from_secs(32),
        "silent router read exceeded the 30-second client budget allowance: {elapsed:?}"
    );
    assert_current_path(&read, "docs/guide.md");
    assert_eq!(read["end"], source.len());
    assert_eq!(read["lines"].as_array().unwrap().len(), 12);
    assert_eq!(
        read["lines"][0]["text"],
        "# SILENT_ROUTER_ACCEPTED row-001\n"
    );
    assert_request_attempts(&silent, "path:docs/guide.md", 1);
    silent.stop();
}

#[test]
fn selected_workspace_member_reads_current_path_before_result_cache_open() {
    let fixture = Fixture::new();
    let (workspace, selected_root) = fixture.write_workspace();
    let blocked_cache = fixture.make_regular_cache_file();
    let blocked_before = fixture.cache_snapshot(&blocked_cache);

    let read = fixture.run_workspace_show(
        &workspace,
        "path:docs/guide.md",
        &blocked_cache,
        true,
        Duration::from_secs(10),
    );
    assert_current_path(&read, "docs/guide.md");
    assert_eq!(read["workspace"], "path-fixture");
    assert_eq!(read["member"], "selected");
    assert_eq!(
        read["repository"],
        selected_root.canonicalize().unwrap().display().to_string()
    );
    assert!(
        read["lines"][0]["text"]
            .as_str()
            .unwrap()
            .contains("selected member marker")
    );
    assert_eq!(fixture.cache_snapshot(&blocked_cache), blocked_before);
}

#[test]
fn stateless_path_continuations_keep_revision_and_original_end() {
    let fixture = Fixture::new();
    let original = fixture.write_markdown(230);
    let blocked_cache = fixture.make_regular_cache_file();

    let first = fixture.run_show(
        "path:docs/guide.md:10-220",
        &blocked_cache,
        true,
        Duration::from_secs(10),
    );
    assert_current_path(&first, "docs/guide.md");
    let cursor = first["next"].as_str().expect("whole read is truncated");
    assert!(cursor.starts_with("read:path:"), "{cursor}");
    let range_end = original
        .split_inclusive(|&byte| byte == b'\n')
        .take(220)
        .map(|line| line.len())
        .sum::<usize>();
    assert_eq!(first["end"], range_end);
    assert!(
        range_end < original.len(),
        "the requested range ends before EOF"
    );
    assert_eq!(first["lines"][0]["line"], 10);
    assert_eq!(first["lines"].as_array().unwrap().len(), 200);

    let continued = fixture.run_show(cursor, &blocked_cache, true, Duration::from_secs(10));
    assert_current_path(&continued, "docs/guide.md");
    assert_eq!(continued["revision"], first["revision"]);
    assert_eq!(continued["end"], first["end"]);
    assert_eq!(continued["lines"].as_array().unwrap().len(), 11);
    assert_eq!(continued["lines"][0]["line"], 210);
    assert_eq!(continued["lines"][10]["line"], 220);
    assert_eq!(continued["truncated"], false);
    assert!(continued["next"].is_null());

    fixture.write_markdown_with_prefix("changed-source", 230);
    let stale = fixture.run_show_failure(cursor, &blocked_cache, true, Duration::from_secs(10));
    assert!(stale.contains("stale_source"), "{stale}");
    assert_eq!(
        fs::read(&blocked_cache).unwrap(),
        b"not a cache directory\n"
    );
}

#[test]
fn current_path_containment_holds_without_daemon_and_after_busy_fallback() {
    let fixture = Fixture::new();
    fixture.write_markdown(20);
    let sentinel = fixture.temp.path().join("outside-secret.txt");
    fs::write(&sentinel, b"CURRENT_PATH_SENTINEL_SECRET").unwrap();
    std::os::unix::fs::symlink(&sentinel, fixture.root.join("outside-link.md")).unwrap();
    let blocked_cache = fixture.make_regular_cache_file();

    for target in [
        format!("path:{}", sentinel.display()),
        "path:../outside-secret.txt".to_owned(),
        "path:outside-link.md".to_owned(),
    ] {
        let failure =
            fixture.run_show_failure(&target, &blocked_cache, true, Duration::from_secs(10));
        assert!(
            !failure.contains("CURRENT_PATH_SENTINEL_SECRET"),
            "{failure}"
        );
        assert!(
            failure.contains("invalid_path") || failure.contains("source_unavailable"),
            "{failure}"
        );
    }

    let mut busy = BusyDaemon::start(&fixture.system_dir, BusyDaemon::busy_reply());
    let _readonly = fixture.make_cache_readonly();
    let default_cache_before = fixture.default_cache_snapshot();
    let failure = fixture.run_show_failure(
        "path:outside-link.md",
        &fixture.cache,
        false,
        Duration::from_secs(25),
    );
    assert!(
        !failure.contains("CURRENT_PATH_SENTINEL_SECRET"),
        "{failure}"
    );
    assert!(failure.contains("source_unavailable"), "{failure}");
    assert_request_attempts(&busy, "path:outside-link.md", BUSY_READ_ATTEMPTS);
    assert!(_readonly.mode_is_read_only());
    assert_eq!(fixture.default_cache_snapshot(), default_cache_before);
    busy.stop();
}

#[test]
fn persistent_busy_falls_back_for_whole_ranged_and_continued_current_paths() {
    let fixture = Fixture::new();
    let original = fixture.write_markdown(240);
    let _readonly = fixture.make_cache_readonly();
    let cache_before = fixture.cache_snapshot(&fixture.cache);
    let default_cache_before = fixture.default_cache_snapshot();
    let mut busy = BusyDaemon::start(&fixture.system_dir, BusyDaemon::busy_reply());

    let whole = fixture.run_show(
        "path:docs/guide.md",
        &fixture.cache,
        false,
        Duration::from_secs(25),
    );
    assert_current_path(&whole, "docs/guide.md");
    assert_eq!(whole["end"], original.len());
    assert_eq!(whole["lines"].as_array().unwrap().len(), 200);
    assert_eq!(whole["truncated"], true);
    assert_request_attempts(&busy, "path:docs/guide.md", BUSY_READ_ATTEMPTS);

    let ranged = fixture.run_show(
        "path:docs/guide.md:10-235",
        &fixture.cache,
        false,
        Duration::from_secs(25),
    );
    assert_current_path(&ranged, "docs/guide.md");
    assert_eq!(ranged["lines"].as_array().unwrap().len(), 200);
    assert_eq!(ranged["lines"][0]["line"], 10);
    assert_eq!(ranged["truncated"], true);
    let cursor = ranged["next"].as_str().expect("ranged read is truncated");
    assert!(cursor.starts_with("read:path:"), "{cursor}");
    assert!(ranged["end"].as_u64().unwrap() < original.len() as u64);
    assert_request_attempts(&busy, "path:docs/guide.md:10-235", BUSY_READ_ATTEMPTS);

    let continued = fixture.run_show(cursor, &fixture.cache, false, Duration::from_secs(25));
    assert_current_path(&continued, "docs/guide.md");
    assert_eq!(continued["revision"], whole["revision"]);
    assert_eq!(continued["end"], ranged["end"]);
    assert_eq!(continued["lines"].as_array().unwrap().len(), 26);
    assert_eq!(continued["lines"][0]["line"], 210);
    assert_eq!(continued["lines"][25]["line"], 235);
    assert_eq!(continued["truncated"], false);
    assert_request_attempts(&busy, cursor, BUSY_READ_ATTEMPTS);

    fixture.write_markdown_with_prefix("changed-source", 240);
    let stale = fixture.run_show_failure(cursor, &fixture.cache, false, Duration::from_secs(25));
    assert!(stale.contains("stale_source"), "{stale}");
    assert_request_attempts(&busy, cursor, BUSY_READ_ATTEMPTS * 2);

    assert_eq!(fixture.cache_snapshot(&fixture.cache), cache_before);
    assert_eq!(fixture.default_cache_snapshot(), default_cache_before);
    assert!(_readonly.mode_is_read_only());
    assert!(busy.all_request_ids_are_fresh());
    assert!(busy.requests_preserve_root_and_cache(&fixture.root, &fixture.cache));
    busy.stop();
}

#[test]
fn query_deadline_answer_does_not_trigger_current_path_fallback() {
    let fixture = Fixture::new();
    fixture.write_markdown_with_prefix("CURRENT_PATH_SENTINEL_SECRET", 12);
    let _readonly = fixture.make_cache_readonly();
    let mut busy = BusyDaemon::start(
        &fixture.system_dir,
        r#"{"status":"failure","message":"timed_out: request deadline elapsed"}"#,
    );

    let failure = fixture.run_show_failure(
        "path:docs/guide.md",
        &fixture.cache,
        false,
        Duration::from_secs(25),
    );
    assert!(failure.contains("timed_out"), "{failure}");
    assert!(
        !failure.contains("CURRENT_PATH_SENTINEL_SECRET"),
        "{failure}"
    );
    assert_eq!(busy.requests().len(), 1, "timeouts are not replayed");
    assert!(busy.all_request_ids_are_fresh());
    busy.stop();
}

#[test]
fn typed_reply_timeout_does_not_return_current_file_bytes() {
    let fixture = Fixture::new();
    fixture.write_markdown_with_prefix("CURRENT_PATH_SENTINEL_SECRET", 12);
    let _readonly = fixture.make_cache_readonly();
    let mut busy = BusyDaemon::start(
        &fixture.system_dir,
        r#"{"status":"failure","message":"daemon_reply_timeout: phase=awaiting_reply wait_ms=30000 deadline_ms=20000 retry_after_ms=250 replay=read_only"}"#,
    );

    let failure = fixture.run_show_failure(
        "path:docs/guide.md",
        &fixture.cache,
        false,
        Duration::from_secs(25),
    );
    assert!(failure.contains("daemon_reply_timeout"), "{failure}");
    assert!(!failure.contains("current_file"), "{failure}");
    assert!(!failure.contains("\"lines\""), "{failure}");
    assert!(
        !failure.contains("CURRENT_PATH_SENTINEL_SECRET"),
        "{failure}"
    );
    let attempts = busy.requests().len();
    assert!(attempts > 0 && attempts <= BUSY_READ_ATTEMPTS, "{attempts}");
    assert!(busy.all_request_ids_are_fresh());
    busy.stop();
}

#[test]
fn incidental_cache_error_text_does_not_trigger_current_path_fallback() {
    let fixture = Fixture::new();
    fixture.write_markdown_with_prefix("CURRENT_PATH_SENTINEL_SECRET", 12);
    let blocked_cache = fixture.make_regular_cache_file();
    let mut busy = BusyDaemon::start(
        &fixture.system_dir,
        r#"{"status":"failure","message":"invalid_body: mentions index_cache_unavailable but is not daemon_busy"}"#,
    );

    let failure = fixture.run_show_failure(
        "path:docs/guide.md",
        &blocked_cache,
        false,
        Duration::from_secs(10),
    );
    assert!(failure.contains("index_cache_unavailable"), "{failure}");
    assert!(!failure.contains("current_file"), "{failure}");
    assert!(!failure.contains("\"lines\""), "{failure}");
    assert!(
        !failure.contains("CURRENT_PATH_SENTINEL_SECRET"),
        "{failure}"
    );
    assert_eq!(
        busy.requests().len(),
        1,
        "only explicit busy enables fallback"
    );
    busy.stop();
}

fn assert_current_path(value: &Value, path: &str) {
    assert_eq!(value["path"], path);
    assert_eq!(value["verified"], false);
    assert_eq!(value["source"], "current_file");
    assert!(value["revision"].as_str().is_some());
}

fn assert_selected_workspace_read(value: &Value, selected_root: &std::path::Path) {
    assert_current_path(value, "docs/guide.md");
    assert_eq!(value["workspace"], "path-fixture");
    assert_eq!(value["member"], "selected");
    assert_eq!(
        value["repository"],
        selected_root.canonicalize().unwrap().display().to_string()
    );
    assert!(
        value["lines"][0]["text"]
            .as_str()
            .unwrap()
            .contains("selected member marker")
    );
}

fn assert_request_attempts(busy: &BusyDaemon, target: &str, attempts: usize) {
    let requests = busy.requests();
    let matching = requests
        .iter()
        .filter(|request| request.args.iter().any(|arg| arg == target))
        .count();
    assert_eq!(matching, attempts, "requests for {target}: {requests:?}");
}
