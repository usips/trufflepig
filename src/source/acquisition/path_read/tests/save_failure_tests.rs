use super::{blocked_cache, page, source_bytes, test_directory};
use crate::{source::show, store::Store};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    sync::Arc,
    time::Duration,
};
use tp_core::{output::OutputBudget, query_deadline::QueryDeadline};

#[test]
fn expired_result_save_deadline_propagates_instead_of_falling_back() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("src.rs"), source_bytes(205)).unwrap();
    let store = Store::unpublished(&root, &directory.path().join("cache"))
        .unwrap()
        .with_query_deadline(QueryDeadline::after(Duration::ZERO));

    let error = show(&store, "path:src.rs", &OutputBudget::new(20_000).unwrap()).unwrap_err();

    assert!(
        tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
}

#[test]
fn dropped_result_save_peer_propagates_cancellation_instead_of_falling_back() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("src.rs"), source_bytes(205)).unwrap();
    let (daemon, peer) = UnixStream::pair().unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::new(daemon));
    drop(peer);
    let store = Store::unpublished(&root, &directory.path().join("cache"))
        .unwrap()
        .with_query_deadline(deadline);

    let error = show(&store, "path:src.rs", &OutputBudget::new(20_000).unwrap()).unwrap_err();

    assert!(
        tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
}

#[test]
fn not_a_database_and_corrupt_result_databases_propagate() {
    for (corrupt_schema, expected_code) in [
        (false, rusqlite::ErrorCode::NotADatabase),
        (true, rusqlite::ErrorCode::DatabaseCorrupt),
    ] {
        let directory = test_directory();
        let root = directory.path().join("repo");
        let cache = directory.path().join("cache");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("src.rs"), source_bytes(205)).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let database = cache.join(crate::results::RESULTS_DATABASE);
        if corrupt_schema {
            // Corrupt a current-layout database, so the failure surfaces in use, not rebuild.
            drop(crate::results::ResultSetStore::open(&cache).unwrap());
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute_batch(
                    "PRAGMA writable_schema=ON;
                     UPDATE sqlite_master SET rootpage=999999 WHERE name='result_sets';",
                )
                .unwrap();
        } else {
            fs::write(&database, b"not a sqlite database").unwrap();
        }
        let store = Store::unpublished(&root, &cache).unwrap();

        let error = show(&store, "path:src.rs", &OutputBudget::new(20_000).unwrap()).unwrap_err();

        assert!(
            !crate::store::is_index_cache_unavailable(&error),
            "corrupt result data is not cache refusal: {error:#}"
        );
        assert!(
            error.chain().any(|cause| cause
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(|error| matches!(error, rusqlite::Error::SqliteFailure(failure, _) if failure.code == expected_code))),
            "expected {expected_code:?}: {error:#}"
        );
    }
}

#[test]
fn generation_parse_and_schema_errors_propagate_before_optional_save() {
    for (invalidation, parse_failure) in [
        (
            "UPDATE meta SET value='not-a-generation' WHERE key='generation'",
            true,
        ),
        ("DROP TABLE meta", false),
    ] {
        let directory = test_directory();
        let root = directory.path().join("repo");
        let cache = directory.path().join("cache");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("src.rs"), b"one line\n").unwrap();
        let store = Store::unpublished(&root, &cache).unwrap();
        store.conn.execute_batch(invalidation).unwrap();

        let error = show(&store, "path:src.rs", &OutputBudget::new(20_000).unwrap()).unwrap_err();

        assert!(
            !crate::store::is_index_cache_unavailable(&error),
            "generation failure is not cache refusal: {error:#}"
        );
        if parse_failure {
            assert!(
                error
                    .chain()
                    .any(|cause| cause.downcast_ref::<std::num::ParseIntError>().is_some()),
                "expected generation parse failure: {error:#}"
            );
        } else {
            assert!(
                format!("{error:#}").contains("no such table: meta"),
                "expected generation schema failure: {error:#}"
            );
        }
        assert!(
            !cache.exists(),
            "generation error reached result persistence"
        );
    }
}

#[test]
fn permission_refused_result_cache_still_uses_stateless_fallback() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let directory = test_directory();
    let root = directory.path().join("repo");
    let cache = directory.path().join("read-only-cache");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("src.rs"), source_bytes(205)).unwrap();
    fs::create_dir(&cache).unwrap();
    let store = Store::unpublished(&root, &cache).unwrap();
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o555)).unwrap();

    let result = show(&store, "path:src.rs", &OutputBudget::new(20_000).unwrap());

    fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
    let output = result.unwrap();
    let page: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert!(page["next"].as_str().unwrap().starts_with("read:path:"));
    assert_eq!(page["source"], "current_file");
}

#[test]
fn blocked_non_directory_cache_keeps_the_existing_stateless_fallback() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("src.rs"), source_bytes(205)).unwrap();
    let cache = blocked_cache(directory.path());
    let store = Store::unpublished(&root, &cache).unwrap();

    let output = page(&store, "path:src.rs");

    assert!(output["next"].as_str().unwrap().starts_with("read:path:"));
    assert_eq!(output["source"], "current_file");
}
