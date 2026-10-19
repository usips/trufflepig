use super::*;
use anyhow::Result;
use fs2::FileExt;
use std::{
    fs::{self, OpenOptions},
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn no_daemon_search(root: &Path, cache: &Path, query: &str) -> Arguments {
    super::super::parse(&[
        "--root".into(),
        root.display().to_string(),
        "--cache".into(),
        cache.display().to_string(),
        "--no-daemon".into(),
        "--format".into(),
        "json".into(),
        "--diagnostics".into(),
        "off".into(),
        "search".into(),
        query.into(),
    ])
    .unwrap()
}

fn no_daemon_history(root: &Path, cache: &Path, verb: &str, target: Option<&str>) -> Arguments {
    let mut args = vec![
        "--root".into(),
        root.display().to_string(),
        "--cache".into(),
        cache.display().to_string(),
        "--no-daemon".into(),
        "--format".into(),
        "json".into(),
        "--diagnostics".into(),
        "off".into(),
        verb.into(),
    ];
    if let Some(target) = target {
        args.push(target.into());
    }
    super::super::parse(&args).unwrap()
}

fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn hold_index_lease(cache: &Path) -> Result<std::fs::File> {
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("index.lock"))?;
    lease.lock_exclusive()?;
    Ok(lease)
}

#[test]
fn actual_no_daemon_read_serves_published_hits_while_another_writer_holds_lease() -> Result<()> {
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    fs::write(root.path().join("lib.rs"), "pub fn published_hit() {}\n")?;
    let mut initial = Store::open(root.path(), cache.path())?;
    initial.index()?;
    drop(initial);
    let lease = hold_index_lease(cache.path())?;

    let started = Instant::now();
    let output = local(
        root.path(),
        cache.path(),
        &no_daemon_search(root.path(), cache.path(), "sym:published_hit"),
        false,
    )?;
    assert!(started.elapsed() < Duration::from_secs(1), "{output}");
    let response: serde_json::Value = serde_json::from_str(&output)?;
    assert_eq!(
        response["hits"].as_array().map(Vec::len),
        Some(1),
        "{response}"
    );
    assert_eq!(response["hits"][0]["name"], "published_hit", "{response}");
    drop(lease);
    Ok(())
}

#[test]
fn actual_no_daemon_cold_read_publishes_after_transient_lease_release() -> Result<()> {
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    fs::write(root.path().join("lib.rs"), "pub fn released_hit() {}\n")?;
    let lease = hold_index_lease(cache.path())?;
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        drop(lease);
    });

    let output = local(
        root.path(),
        cache.path(),
        &no_daemon_search(root.path(), cache.path(), "sym:released_hit"),
        false,
    )?;
    releaser.join().expect("lease releaser thread must finish");
    let response: serde_json::Value = serde_json::from_str(&output)?;
    assert_eq!(
        response["hits"].as_array().map(Vec::len),
        Some(1),
        "{response}"
    );
    assert_eq!(response["hits"][0]["name"], "released_hit", "{response}");
    assert_eq!(response["generation"], 1, "{response}");
    assert!(cache.path().join("index.sqlite3").is_file());
    Ok(())
}

#[test]
fn actual_history_read_on_unseeded_cache_creates_no_index() -> Result<()> {
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    run_git(root.path(), &["init", "-q"]);
    run_git(
        root.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    run_git(root.path(), &["config", "user.name", "Test"]);
    fs::write(root.path().join("a.rs"), "fn seeded() {}\n")?;
    run_git(root.path(), &["add", "."]);
    run_git(root.path(), &["commit", "-qm", "root"]);

    let status = local(
        root.path(),
        cache.path(),
        &no_daemon_history(root.path(), cache.path(), "hist-status", None),
        false,
    )?;
    let status: serde_json::Value = serde_json::from_str(&status)?;
    assert_eq!(status["complete"], false, "{status}");
    assert!(
        !cache.path().join("index.sqlite3").exists(),
        "hist-status must not create the live index"
    );

    let hist = local(
        root.path(),
        cache.path(),
        &no_daemon_history(root.path(), cache.path(), "hist", Some("a.rs")),
        false,
    )?;
    let hist: serde_json::Value = serde_json::from_str(&hist)?;
    assert!(hist["hits"].is_array(), "{hist}");
    assert!(
        !cache.path().join("index.sqlite3").exists(),
        "hist must not create the live index"
    );
    Ok(())
}
