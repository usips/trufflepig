use super::*;
use anyhow::Result;
use fs2::FileExt;
use std::time::{Duration, Instant};

#[test]
fn bounded_reconcile_expiry_preserves_the_previous_publication() -> Result<()> {
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    std::fs::write(root.path().join("existing.rs"), "pub fn existing() {}\n")?;
    let mut initial = Store::open(root.path(), cache.path())?;
    initial.index()?;
    let generation = initial.generation()?;
    let coverage = serde_json::to_value(initial.coverage()?)?;
    drop(initial);

    crate::query_deadline::write_slow_reconcile_fixture(root.path(), "later");
    let deadline = QueryDeadline::after(Duration::from_millis(100));
    let started = Instant::now();
    let error = Store::open_for_reconcile(root.path(), cache.path(), deadline.clone())
        .and_then(|mut store| store.index_until(deadline).map(|_| store))
        .err()
        .expect("the slow reconcile must expire");
    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(2), "{error:#}");

    let published = Store::open_read(root.path(), cache.path(), QueryDeadline::start())?;
    assert_eq!(published.generation()?, generation);
    assert_eq!(serde_json::to_value(published.coverage()?)?, coverage);
    Ok(())
}

#[test]
fn cold_index_waits_only_for_maintenance_slice_behind_held_writer_lease() -> Result<()> {
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    std::fs::write(root.path().join("lib.rs"), "pub fn leased() {}\n")?;
    let mut store = Store::open_for_reconcile(root.path(), cache.path(), QueryDeadline::start())?;
    let lease = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.path().join("index.lock"))?;
    lease.lock_exclusive()?;

    let started = Instant::now();
    assert!(
        store
            .index_available_until(QueryDeadline::start())?
            .is_none()
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(store.generation()?, 0);

    let options = super::super::parse(&[
        "--no-daemon".to_owned(),
        "search".to_owned(),
        "sym:leased".to_owned(),
    ])?;
    let query_started = Instant::now();
    let error = read_store(
        root.path(),
        cache.path(),
        &options,
        false,
        &QueryDeadline::start(),
    )
    .err()
    .expect("a held writer lease must return warming");
    assert!(is_index_warming(&error), "{error:#}");
    assert!(
        error
            .to_string()
            .contains("no retained checkpoint progress"),
        "{error:#}"
    );
    assert!(error.to_string().contains("trufflepig index"), "{error:#}");
    assert!(
        query_started.elapsed() < Duration::from_secs(3),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn invalid_cache_error_names_the_selected_path_and_recovery_options() -> Result<()> {
    let root = tempfile::tempdir()?;
    let scratch = tempfile::tempdir()?;
    let cache = scratch.path().join("cache-file");
    std::fs::write(&cache, "not a directory")?;
    let options = super::super::parse(&[
        "--no-daemon".to_owned(),
        "search".to_owned(),
        "sym:missing".to_owned(),
    ])?;
    let error = read_store(
        root.path(),
        &cache,
        &options,
        false,
        &QueryDeadline::start(),
    )
    .err()
    .expect("the path used as a cache must be rejected");
    assert!(is_index_cache_unavailable(&error), "{error:#}");
    let text = format!("{error:#}");
    assert!(text.contains(&cache.display().to_string()), "{text}");
    assert!(text.contains("--cache"), "{text}");
    assert!(text.contains("XDG_CACHE_HOME"), "{text}");
    assert!(text.contains("show path:FILE[:START-END]"), "{text}");
    Ok(())
}

#[test]
fn no_daemon_map_reports_selected_cache_and_cache_free_source_fallback() -> Result<()> {
    let root = tempfile::tempdir()?;
    let scratch = tempfile::tempdir()?;
    std::fs::write(root.path().join("lib.rs"), "pub fn indexed() {}\n")?;
    let cache = scratch.path().join("cache-file");
    let marker = b"preserve this selected cache path\n";
    std::fs::write(&cache, marker)?;
    let options = super::super::parse(&[
        "--root".to_owned(),
        root.path().display().to_string(),
        "--cache".to_owned(),
        cache.display().to_string(),
        "--no-daemon".to_owned(),
        "--diagnostics".to_owned(),
        "off".to_owned(),
        "map".to_owned(),
        "lib.rs".to_owned(),
    ])?;

    let output = local(root.path(), &cache, &options, false)?;
    let page: serde_json::Value = serde_json::from_str(&output)?;
    assert!(!page["hits"].as_array().unwrap().is_empty(), "{page}");
    assert_eq!(page["cache_fallback"]["status"], "index_cache_unavailable");
    assert_eq!(page["cache_fallback"]["cache"], cache.display().to_string());
    assert_eq!(
        std::fs::read(&cache)?.as_slice(),
        &marker[..],
        "cache identity remains selected"
    );
    Ok(())
}

#[test]
fn expired_read_does_not_create_a_cache_or_start_reconciliation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let scratch = tempfile::tempdir()?;
    let cache = scratch.path().join("not-created");
    let options = super::super::parse(&[
        "--no-daemon".to_owned(),
        "search".to_owned(),
        "sym:missing".to_owned(),
    ])?;
    let deadline = QueryDeadline::after(Duration::ZERO);

    let error = read_store(root.path(), &cache, &options, false, &deadline)
        .err()
        .expect("an expired read must not reconcile");
    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn status_reports_warming_without_creating_an_index() -> Result<()> {
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    std::fs::write(root.path().join("lib.rs"), "pub fn early() {}\n")?;
    let lease = std::fs::File::create(cache.path().join("index.lock"))?;
    lease.lock_exclusive()?;
    let options = super::super::parse(&[
        "--root".to_owned(),
        root.path().display().to_string(),
        "--format".to_owned(),
        "json".to_owned(),
        "--diagnostics".to_owned(),
        "off".to_owned(),
        "status".to_owned(),
    ])?;

    let output = super::super::local(root.path(), cache.path(), &options, false)?;
    let status: serde_json::Value = serde_json::from_str(&output)?;
    assert_eq!(status["state"], "warming", "{status}");
    assert!(!cache.path().join("index.sqlite3").exists());
    Ok(())
}

#[test]
fn doctor_uses_an_unpublished_view_when_the_writer_lease_is_held() -> Result<()> {
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    std::fs::write(root.path().join("lib.rs"), "pub fn early() {}\n")?;
    let lease = std::fs::File::create(cache.path().join("index.lock"))?;
    lease.lock_exclusive()?;
    let options = super::super::parse(&[
        "--root".to_owned(),
        root.path().display().to_string(),
        "--format".to_owned(),
        "json".to_owned(),
        "--diagnostics".to_owned(),
        "off".to_owned(),
        "doctor".to_owned(),
    ])?;

    let output = super::super::local(root.path(), cache.path(), &options, true)?;
    let _doctor: serde_json::Value = serde_json::from_str(&output)?;
    assert!(!cache.path().join("index.sqlite3").exists());
    Ok(())
}
