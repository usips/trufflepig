use super::super::acquire;
use super::{await_republish, liveness_probe_result};
use crate::daemon::deadline::QueryDeadline;
use crate::search::InvocationDirectory;
use crate::source::line_span;
use crate::{output::OutputBudget, store::Store};
use std::{
    os::unix::net::{UnixListener, UnixStream},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

struct ServingDaemonFixture {
    _root: tempfile::TempDir,
    _cache: tempfile::TempDir,
    listener: UnixListener,
    store: Store,
    generation: i64,
}

fn serving_daemon_fixture() -> ServingDaemonFixture {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn target() {}\n").unwrap();
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let generation = store.generation().unwrap();
    let listener = UnixListener::bind(cache.path().join(crate::daemon::SOCKET_NAME)).unwrap();
    ServingDaemonFixture {
        _root: root,
        _cache: cache,
        listener,
        store,
        generation,
    }
}

#[test]
fn sym_read_falls_back_to_current_bytes() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn target() {}\n").unwrap();
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let published = acquire(&store, "sym:target", None, &InvocationDirectory::root()).unwrap();
    assert!(published.verified);
    std::fs::write(
        root.path().join("lib.rs"),
        "// moved down\n\nfn target() -> u8 { 1 }\n",
    )
    .unwrap();

    let current = acquire(&store, "sym:target", None, &InvocationDirectory::root()).unwrap();
    assert!(!current.verified);
    assert_eq!(current.freshness, Some("unpublished"));
    assert_eq!(
        &current.bytes[current.span.start..current.span.end],
        b"fn target() -> u8 { 1 }"
    );
    assert_eq!(
        current
            .definitions
            .as_ref()
            .map(|selection| selection.declarations),
        Some(1)
    );
    let rendered: serde_json::Value = serde_json::from_str(
        &crate::source::show(&store, "sym:target", &OutputBudget::new(4_000).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(rendered["freshness"], "unpublished");
    assert_eq!(rendered["verified"], false);
    assert_eq!(rendered["lines"][0]["line"], 3);
    // Handles stay immutable: the published read's handle is stale now.
    let error = acquire(
        &store,
        &published.handle,
        None,
        &InvocationDirectory::root(),
    )
    .err()
    .unwrap();
    assert!(error.to_string().starts_with("stale_source:"), "{error:#}");

    std::fs::write(root.path().join("lib.rs"), b"fn target() {}\0").unwrap();
    let binary = acquire(&store, "sym:target", None, &InvocationDirectory::root())
        .err()
        .unwrap();
    let message = format!("{binary:#}");
    assert!(message.starts_with("stale_source:"), "{message}");
    assert!(message.contains("source_excluded"), "{message}");

    std::fs::write(
        root.path().join("lib.rs"),
        "// moved down\n\nfn target() -> u8 { 1 }\n",
    )
    .unwrap();
    store.index().unwrap();
    let republished = acquire(&store, "sym:target", None, &InvocationDirectory::root()).unwrap();
    assert!(republished.verified);
    assert_eq!(republished.freshness, None);
}

#[test]
fn stale_qualified_symbol_stays_in_the_selected_method_container() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("lib.rs"),
        "struct A;\nimpl A {\n    fn open() { 10 }\n}\nstruct B;\nimpl B {\n    fn open() { 20 }\n}\n",
    )
    .unwrap();
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let original = acquire(&store, "sym:A::open", None, &InvocationDirectory::root()).unwrap();
    assert!(original.verified);
    assert_eq!(
        &original.bytes[original.span.start..original.span.end],
        b"fn open() { 10 }"
    );

    // Put B's same-named method at A's old line; nearest-name lookup alone
    // would return this body for the still-qualified `sym:A::open` query.
    std::fs::write(
        root.path().join("lib.rs"),
        "struct B;\nimpl B {\n    fn open() { 99 }\n}\nstruct A;\nimpl A {\n    fn open() { 11 }\n}\n",
    )
    .unwrap();
    let current = acquire(&store, "sym:A::open", None, &InvocationDirectory::root()).unwrap();
    assert!(!current.verified);
    assert_eq!(current.freshness, Some("unpublished"));
    let body = &current.bytes[current.span.start..current.span.end];
    assert_eq!(body, b"fn open() { 11 }");
    assert!(!body.windows(2).any(|window| window == b"99"));
    assert_eq!(
        line_span(&current.bytes, current.span.start, current.span.end).0,
        7
    );
}

#[test]
fn stale_republish_wait_obeys_query_deadline() {
    let fixture = serving_daemon_fixture();
    let store = fixture
        .store
        .with_query_deadline(QueryDeadline::after(Duration::from_millis(40)));
    let started = Instant::now();

    let error = await_republish(&store, fixture.generation).unwrap_err();

    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert!(started.elapsed() < Duration::from_millis(250));
    assert_eq!(store.generation().unwrap(), fixture.generation);
    drop(fixture.listener);
}

#[test]
fn stale_republish_probe_preserves_daemon_liveness_semantics() {
    let active_deadline = QueryDeadline::after(Duration::from_secs(5));
    let probe_timeout = anyhow::anyhow!(coded!(TimedOut, "phase=connecting"));

    assert!(liveness_probe_result(Err(probe_timeout), &active_deadline).unwrap());

    let expired_deadline = QueryDeadline::after(Duration::ZERO);
    let caller_timeout = anyhow::anyhow!(coded!(TimedOut, "phase=connecting"));
    let error = liveness_probe_result(Err(caller_timeout), &expired_deadline).unwrap_err();
    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );

    let unavailable_deadline = QueryDeadline::after(Duration::from_secs(5));
    let unavailable = anyhow::anyhow!("connect to repository daemon: permission denied");
    assert!(!liveness_probe_result(Err(unavailable), &unavailable_deadline).unwrap());
}

#[test]
fn stale_republish_wait_observes_cancellation_promptly() {
    let fixture = serving_daemon_fixture();
    let (cancelled_peer, disconnect_peer) = UnixStream::pair().unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::new(cancelled_peer));
    let store = fixture.store.with_query_deadline(deadline);
    let cancel = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        drop(disconnect_peer);
    });
    let started = Instant::now();

    let error = await_republish(&store, fixture.generation).unwrap_err();

    cancel.join().unwrap();
    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert!(started.elapsed() < Duration::from_millis(250));
    assert_eq!(store.generation().unwrap(), fixture.generation);
    drop(fixture.listener);
}

#[test]
fn stale_republish_wait_accepts_a_later_generation_before_query_deadline() {
    let fixture = serving_daemon_fixture();
    let root = fixture._root.path().to_path_buf();
    let cache = fixture._cache.path().to_path_buf();
    let store = fixture
        .store
        .with_query_deadline(QueryDeadline::after(Duration::from_secs(5)));
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        std::fs::write(root.join("lib.rs"), "fn target() -> u8 { 1 }\n").unwrap();
        let mut store = Store::open(&root, &cache).unwrap();
        store.index().unwrap();
    });

    let started = Instant::now();
    let republished = await_republish(&store, fixture.generation).unwrap();

    writer.join().unwrap();
    assert!(republished);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(store.generation().unwrap() > fixture.generation);
    drop(fixture.listener);
}

#[test]
fn stale_republish_wait_propagates_generation_database_errors() {
    let fixture = serving_daemon_fixture();
    fixture.store.conn.execute_batch("DROP TABLE meta").unwrap();

    let error = await_republish(&fixture.store, fixture.generation).unwrap_err();

    assert!(
        error
            .chain()
            .any(|cause| cause.downcast_ref::<rusqlite::Error>().is_some()),
        "generation database error must remain typed: {error:#}"
    );
    drop(fixture.listener);
}
