use super::{LiveRegexCheckpoint, check_live_deadline, live_regex, live_regex_with_checkpoint};
use crate::{search::Query, store::Store};
use anyhow::Result;
use std::{os::unix::net::UnixStream, path::Path, sync::Arc, time::Duration};
use tp_core::query_deadline::QueryDeadline;

#[test]
fn binary_files_are_excluded_not_unsearched() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn needle() {}\n").unwrap();
    std::fs::write(root.path().join("blob.bin"), b"needle\0\x01").unwrap();
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let query = Query::parse("re:needle").unwrap();
    let found = crate::search::search(&store, &query, false, cache.path()).unwrap();
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.coverage["live_read_failures"], 0);
    assert_eq!(found.coverage["live_excluded_files"], 1);
}

#[test]
fn before_compile_timeouts_precede_invalid_regex_errors() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "needle\n").unwrap();
    let store = Store::open(root.path(), cache.path()).unwrap();
    let query = Query::parse("re:(").unwrap();
    let paths = query.path.bind(&store.conn).unwrap();
    let mut hits = Vec::new();
    let mut coverage = serde_json::json!({});
    let mut truncated = false;

    let ordinary_error = live_regex(
        &store,
        &query,
        &paths,
        cache.path(),
        &mut hits,
        &mut coverage,
        &mut truncated,
        &QueryDeadline::start(),
    )
    .unwrap_err();
    assert!(!ordinary_error.to_string().starts_with("timed_out:"));
    assert!(!ordinary_error.to_string().contains("scan incomplete"));

    let error = live_regex(
        &store,
        &query,
        &paths,
        cache.path(),
        &mut hits,
        &mut coverage,
        &mut truncated,
        &QueryDeadline::after(Duration::ZERO),
    )
    .unwrap_err();

    assert!(error.to_string().starts_with("timed_out:"));
    assert!(
        error
            .to_string()
            .contains("scan incomplete; discarded 0 partial hits")
    );
    assert!(hits.is_empty());
    assert_eq!(coverage, serde_json::json!({}));

    let (client, server) = UnixStream::pair().unwrap();
    drop(client);
    let deadline = QueryDeadline::start().with_peer(Arc::new(server));
    let error = live_regex(
        &store,
        &query,
        &paths,
        cache.path(),
        &mut hits,
        &mut coverage,
        &mut truncated,
        &deadline,
    )
    .unwrap_err();

    assert!(error.to_string().starts_with("timed_out:"));
    assert!(hits.is_empty());
}

#[test]
fn files_without_matches_skip_hash_and_extraction() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "fn needle() {}\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "fn unrelated() {}\n").unwrap();
    let store = Store::open(root.path(), cache.path()).unwrap();
    let query = Query::parse("re:needle kind:function").unwrap();
    let paths = query.path.bind(&store.conn).unwrap();
    let mut hits = Vec::new();
    let mut coverage = serde_json::json!({});
    let mut truncated = false;
    let mut hashed = Vec::new();
    let mut extracted = Vec::new();
    let mut checkpoint = |path: &Path, stage: LiveRegexCheckpoint| {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        match stage {
            LiveRegexCheckpoint::BeforeHash => hashed.push(name),
            LiveRegexCheckpoint::BeforeExtract => extracted.push(name),
            _ => {}
        }
        Ok(())
    };

    live_regex_with_checkpoint(
        &store,
        &query,
        &paths,
        cache.path(),
        &mut hits,
        &mut coverage,
        &mut truncated,
        &mut checkpoint,
    )
    .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hashed, ["a.rs"]);
    assert_eq!(extracted, ["a.rs"]);
}

#[test]
fn timeout_between_matching_files_returns_error_after_partial_hit() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "needle\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "needle\n").unwrap();
    let store = Store::open(root.path(), cache.path()).unwrap();
    let query = Query::parse("re:needle").unwrap();
    let paths = query.path.bind(&store.conn).unwrap();
    let mut hits = Vec::new();
    let mut coverage = serde_json::json!({});
    let mut truncated = false;
    let (client, server) = UnixStream::pair().unwrap();
    let mut client = Some(client);
    let deadline = QueryDeadline::start().with_peer(Arc::new(server));
    let mut checkpoint = |path: &Path, stage: LiveRegexCheckpoint| {
        if stage == LiveRegexCheckpoint::RegexMatch
            && path.file_name().is_some_and(|name| name == "a.rs")
        {
            check_live_deadline(&deadline, stage)?;
            drop(client.take());
            return Ok(());
        }
        check_live_deadline(&deadline, stage)
    };

    let error = live_regex_with_checkpoint(
        &store,
        &query,
        &paths,
        cache.path(),
        &mut hits,
        &mut coverage,
        &mut truncated,
        &mut checkpoint,
    )
    .unwrap_err();

    assert!(error.to_string().starts_with("timed_out:"));
    assert!(
        error
            .to_string()
            .contains("scan incomplete; discarded 1 partial hits")
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "a.rs");
}

#[test]
fn peer_drop_inside_match_loop_waits_until_the_64th_match_poll() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("many.rs"), "needle\n".repeat(128)).unwrap();
    let store = Store::open(root.path(), cache.path()).unwrap();
    let query = Query::parse("re:needle").unwrap();
    let paths = query.path.bind(&store.conn).unwrap();
    let mut hits = Vec::new();
    let mut coverage = serde_json::json!({});
    let mut truncated = false;
    let mut match_iterations = 0;
    let mut match_polls = 0;
    let (client, server) = UnixStream::pair().unwrap();
    let mut client = Some(client);
    let deadline = QueryDeadline::start().with_peer(Arc::new(server));
    let mut checkpoint = |_: &Path, stage: LiveRegexCheckpoint| -> Result<()> {
        if stage == LiveRegexCheckpoint::RegexMatch {
            check_live_deadline(&deadline, stage)?;
            match_iterations += 1;
            if match_iterations == 1 {
                drop(client.take());
            }
            return Ok(());
        }
        if stage == LiveRegexCheckpoint::MatchBatch {
            match_polls += 1;
        }
        check_live_deadline(&deadline, stage)
    };

    let error = live_regex_with_checkpoint(
        &store,
        &query,
        &paths,
        cache.path(),
        &mut hits,
        &mut coverage,
        &mut truncated,
        &mut checkpoint,
    )
    .unwrap_err();

    assert!(error.to_string().starts_with("timed_out:"));
    assert!(
        error
            .to_string()
            .contains("scan incomplete; discarded 63 partial hits")
    );
    assert_eq!(match_iterations, 64);
    assert_eq!(match_polls, 1);
    assert_eq!(hits.len(), 63);
}

#[test]
fn peer_drop_after_the_last_match_does_not_reject_a_completed_scan() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("only.rs"), "needle\n").unwrap();
    let store = Store::open(root.path(), cache.path()).unwrap();
    let query = Query::parse("re:needle").unwrap();
    let paths = query.path.bind(&store.conn).unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let mut client = Some(client);
    let deadline = QueryDeadline::start().with_peer(Arc::new(server));
    let mut hits = Vec::new();
    let mut coverage = serde_json::json!({});
    let mut truncated = false;
    let mut checkpoint = |path: &Path, stage: LiveRegexCheckpoint| {
        check_live_deadline(&deadline, stage)?;
        if stage == LiveRegexCheckpoint::RegexMatch {
            assert_eq!(path.file_name().unwrap(), "only.rs");
            drop(client.take());
        }
        Ok(())
    };

    live_regex_with_checkpoint(
        &store,
        &query,
        &paths,
        cache.path(),
        &mut hits,
        &mut coverage,
        &mut truncated,
        &mut checkpoint,
    )
    .unwrap();

    assert!(deadline.expired());
    assert_eq!(hits.len(), 1);
    assert_eq!(coverage["live_checked_files"], 1);
    assert!(!truncated);
}

#[test]
fn clock_expiry_after_the_last_match_does_not_reject_a_completed_scan() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("only.rs"), "needle\n").unwrap();
    let store = Store::open(root.path(), cache.path()).unwrap();
    let query = Query::parse("re:needle").unwrap();
    let paths = query.path.bind(&store.conn).unwrap();
    let mut deadline = QueryDeadline::after(Duration::from_secs(60));
    let mut final_match_seen = false;
    let mut clock_expired_after_match = false;
    let mut hits = Vec::new();
    let mut coverage = serde_json::json!({});
    let mut truncated = false;
    let mut checkpoint = |path: &Path, stage: LiveRegexCheckpoint| {
        check_live_deadline(&deadline, stage)?;
        if stage == LiveRegexCheckpoint::RegexMatch {
            assert_eq!(path.file_name().unwrap(), "only.rs");
            assert!(!final_match_seen);
            final_match_seen = true;

            deadline = deadline.capped(Duration::from_millis(5));
            while !deadline.time_expired() {
                std::hint::spin_loop();
            }
            clock_expired_after_match = true;
        }
        Ok(())
    };

    live_regex_with_checkpoint(
        &store,
        &query,
        &paths,
        cache.path(),
        &mut hits,
        &mut coverage,
        &mut truncated,
        &mut checkpoint,
    )
    .unwrap();
    drop(checkpoint);

    assert!(final_match_seen);
    assert!(clock_expired_after_match);
    assert!(deadline.time_expired());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "only.rs");
    assert_eq!(coverage["live_checked_files"], 1);
    assert!(!truncated);
}

#[test]
fn prepared_search_propagates_live_scan_cancellation_as_an_error() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "needle\n").unwrap();
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let query = Query::parse("re:needle").unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    drop(client);
    let deadline = QueryDeadline::start().with_peer(Arc::new(server));

    let result = crate::search::search_prepared(
        &store,
        &query,
        cache.path(),
        None,
        None,
        &mut crate::search::telemetry::RetrievalTrace::disabled(),
        &deadline,
    );

    let error = result.unwrap_err();
    assert!(error.to_string().starts_with("timed_out:"), "{error:#}");
}

#[test]
fn peer_drop_is_polled_while_filters_reject_every_walked_file() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for file in 0..2_048 {
        std::fs::write(root.path().join(format!("f{file}.rs")), "needle\n").unwrap();
    }
    let store = Store::open(root.path(), cache.path()).unwrap();
    // Every file is Rust, so the typescript filter rejects each one before its read.
    let query = Query::parse("re:needle lang:typescript").unwrap();
    let paths = query.path.bind(&store.conn).unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let mut client = Some(client);
    let deadline = QueryDeadline::start().with_peer(Arc::new(server));
    let mut walked = 0;
    let mut reads = 0;
    let mut hits = Vec::new();
    let mut coverage = serde_json::json!({});
    let mut truncated = false;
    let mut checkpoint = |_: &Path, stage: LiveRegexCheckpoint| {
        match stage {
            LiveRegexCheckpoint::WalkEntry => {
                walked += 1;
                if walked == 1 {
                    drop(client.take());
                }
            }
            LiveRegexCheckpoint::BeforeRead => reads += 1,
            _ => {}
        }
        check_live_deadline(&deadline, stage)
    };

    let error = live_regex_with_checkpoint(
        &store,
        &query,
        &paths,
        cache.path(),
        &mut hits,
        &mut coverage,
        &mut truncated,
        &mut checkpoint,
    )
    .unwrap_err();
    drop(checkpoint);

    assert!(error.to_string().starts_with("timed_out:"), "{error:#}");
    assert_eq!(reads, 0);
    assert_eq!(
        walked, 1024,
        "the first walk poll must observe the peer drop"
    );
    assert!(hits.is_empty());
}
