use super::*;
use anyhow::Result;
use fs2::FileExt;
use std::{
    fs::{self, OpenOptions},
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
