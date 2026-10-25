use super::*;
use crate::diagnostics::Operation;
use serde_json::json;
use tp_core::request_context::RequestContext;

#[test]
fn diff_and_later_pages_preserve_preimage_identity() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let store = tp_index::store::Store::open(root.path(), cache.path()).unwrap();
    let oid = tp_core::identity::GitOid::parse(&"a".repeat(40)).unwrap();
    let revision = tp_core::identity::ContentRevision::of(b"historical source");
    let id = results::save_entries(
        &store,
        1,
        json!({}),
        vec![ResultEntry::Change(tp_index::results::ChangeEntry {
            handle: String::new(),
            name: "alpha".into(),
            status: "modified".into(),
            before: Some(HistoricalSource {
                repository: encode_path(&root.path().join(".git")),
                commit: oid,
                blob: oid,
                revision,
                path: "a.rs".into(),
                span: tp_core::identity::ByteSpan::new(10, 40).unwrap(),
            }),
            after: None,
            correspondence: "exact".into(),
        })],
        false,
    )
    .unwrap();
    let handle = format!("{id}:1");
    let source = json!({"repository":"/repo/.git","path":"a.rs","revision":revision,"commit":oid,"blob":oid,"span":{"start":10,"end":40}});
    let response = json!({"hits":[{"entry":"change","handle":handle,"before":source}],"hunks":[{"handle":handle,"before":{"start":12,"end":18,"text":"secret source"}}]});
    let mut event = RequestEvent::new(
        RequestContext::new(None, None),
        Operation::Diff,
        Outcome::Success,
    );
    capture_emitted(
        root.path(),
        &response.to_string(),
        &mut event,
        Some(&SavedEntries::singleton(root.path(), cache.path())),
        None,
    );
    assert_eq!(event.emitted.len(), 2);
    assert_eq!(event.emitted[0].original_rank, Some(1));
    assert_eq!(event.emitted[1].content_revision, revision.to_string());
    assert_eq!(
        (event.emitted[1].start_byte, event.emitted[1].end_byte),
        (12, 18)
    );
    assert!(event.emitted[1].source_body);
    assert_eq!(event.emitted[1].blob.as_deref(), Some(oid.as_str()));
    assert!(
        !serde_json::to_string(&event)
            .unwrap()
            .contains("secret source")
    );
}

#[test]
fn candidate_metadata_and_capture_loss_are_explicit() {
    let candidates: Vec<_> = (0..40)
        .map(|n| json!({"path":"a.rs","revision":"rev","start":n,"end":n+1}))
        .collect();
    let response = json!({"hits":[{"path":"b.rs","revision":"rev2","start":0,"end":10,"candidates":candidates}]});
    let mut event = RequestEvent::new(
        RequestContext::new(None, None),
        Operation::References,
        Outcome::Success,
    );
    capture_emitted(
        std::path::Path::new("/repo"),
        &response.to_string(),
        &mut event,
        None,
        None,
    );
    assert!(event.emitted.is_empty());
}

#[test]
fn saved_handles_preserve_identity_and_report_unresolved_handles() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn alpha() {}\n").unwrap();
    let mut store = tp_index::store::Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let revision = tp_core::identity::ContentRevision::of(b"fn alpha() {}\n").to_string();
    let id = results::save_entries(
        &store,
        1,
        json!({}),
        vec![ResultEntry::LiveSource(tp_index::results::Hit {
            handle: String::new(),
            path: "lib.rs".into(),
            revision: Some(revision.clone()),
            start: 3,
            end: 8,
            start_line: 1,
            end_line: 1,
            name: "alpha".into(),
            kind: "function".into(),
            container: None,
            provenance: Some("exact_identifier".into()),
            resolution: None,
            candidates: Vec::new(),
            target: None,
            repeats: None,
            snippet: None,
            differs: false,
        })],
        false,
    )
    .unwrap();
    let saved = SavedEntries::singleton(root.path(), cache.path());
    let mut event = RequestEvent::new(
        RequestContext::new(None, None),
        Operation::Search,
        Outcome::Success,
    );
    let response = json!({
        "hits":[
            {"handle":format!("{id}:1"),"file":"file:///wrong.rs","start_line":99,"end_line":99},
            {"handle":"0123456789abcdef0123456789abcdef:1","file":"file:///unpersisted.rs","start_line":1,"end_line":1}
        ]
    });
    capture_emitted(
        root.path(),
        &response.to_string(),
        &mut event,
        Some(&saved),
        None,
    );
    assert_eq!(event.emitted.len(), 1);
    assert_eq!(event.emitted[0].path, "lib.rs");
    assert_eq!(event.emitted[0].content_revision, revision);
    assert_eq!(
        (event.emitted[0].start_byte, event.emitted[0].end_byte),
        (3, 8)
    );
    assert!(event.truncated);
}

fn handle(ordinal: usize) -> String {
    format!("00000000000000000000000000000000:{ordinal}")
}

fn source_entry(rank: usize) -> ResolvedEntry {
    ResolvedEntry {
        owner_root: "/repo".into(),
        member: None,
        member_rank: rank,
        entry: ResultEntry::LiveSource(tp_index::results::Hit {
            handle: String::new(),
            path: format!("src/{rank}.rs"),
            revision: Some("saved-revision".into()),
            start: 0,
            end: 4,
            start_line: 1,
            end_line: 1,
            name: "saved".into(),
            kind: "function".into(),
            container: None,
            provenance: None,
            resolution: None,
            candidates: Vec::new(),
            target: None,
            repeats: None,
            snippet: None,
            differs: false,
        }),
    }
}

fn event() -> RequestEvent {
    RequestEvent::new(
        RequestContext::new(None, None),
        Operation::Search,
        Outcome::Success,
    )
}

#[test]
fn later_expiry_marks_previously_captured_json_hit_partial() {
    let response = json!({
        "hits": [{
            "handle": handle(1),
            "path": "src/1.rs",
            "before": {"path": "src/1.rs"}
        }]
    });
    let mut event = event();
    let mut lookups = 0;
    let mut resolve = |handle: &ResultHandle| {
        lookups += 1;
        if lookups == 1 {
            Ok(source_entry(handle.ordinal))
        } else {
            Err(LookupFailure::Expired)
        }
    };

    capture_emitted_with_resolver(
        Path::new("/repo"),
        &response.to_string(),
        &mut event,
        None,
        &mut resolve,
    );

    assert_eq!(lookups, 2);
    assert_eq!(event.emitted.len(), 1);
    assert!(event.truncated);
    assert!(matches!(event.outcome, Outcome::Success));
}

#[test]
fn complete_json_identity_capture_does_not_mark_truncated() {
    let response = json!({
        "hits": [
            {"handle": handle(1), "path": "wrong/one.rs"},
            {"handle": handle(2), "file": "wrong/two.rs"}
        ]
    });
    let mut event = event();
    let mut resolve = |handle: &ResultHandle| Ok(source_entry(handle.ordinal));

    capture_emitted_with_resolver(
        Path::new("/repo"),
        &response.to_string(),
        &mut event,
        None,
        &mut resolve,
    );

    assert_eq!(event.emitted.len(), 2);
    assert!(!event.truncated);
}

#[test]
fn missing_result_store_marks_expected_json_identity_incomplete() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let saved = SavedEntries::singleton(root.path(), cache.path());
    let response = json!({
        "hits": [{"handle": handle(1), "path": "src/1.rs"}]
    });
    let mut event = event();

    capture_emitted(
        root.path(),
        &response.to_string(),
        &mut event,
        Some(&saved),
        None,
    );

    assert!(event.emitted.is_empty());
    assert!(event.truncated);
    assert!(matches!(event.outcome, Outcome::Success));
}

#[test]
fn relationship_identity_lookup_loss_marks_capture_incomplete() {
    let response = json!({
        "relationships": [{
            "source": {"handle": handle(1), "path": "src/1.rs"},
            "target": {"handle": handle(2), "path": "src/2.rs"}
        }]
    });
    let mut event = event();
    let mut resolve = |_handle: &ResultHandle| Err(LookupFailure::Unavailable);

    capture_emitted_with_resolver(
        Path::new("/repo"),
        &response.to_string(),
        &mut event,
        None,
        &mut resolve,
    );

    assert!(event.emitted.is_empty());
    assert!(event.truncated);
}

#[test]
fn commit_handles_and_malformed_values_have_no_source_loss() {
    let response = json!({
        "hits": [
            {"handle": "malformed", "path": "unrelated.rs"},
            {"handle": handle(1), "path": "commit"}
        ]
    });
    let mut event = event();
    let mut calls = 0;
    let oid = tp_core::identity::GitOid::parse(&"a".repeat(40)).unwrap();
    let mut resolve = |_handle: &ResultHandle| {
        calls += 1;
        Ok(ResolvedEntry {
            owner_root: "/repo".into(),
            member: None,
            member_rank: 1,
            entry: ResultEntry::Commit(tp_index::results::CommitEntry {
                handle: String::new(),
                repository: "/repo/.git".into(),
                commit: oid,
                parent: None,
                summary: "change".into(),
                committer_time: 0,
            }),
        })
    };

    capture_emitted_with_resolver(
        Path::new("/repo"),
        &response.to_string(),
        &mut event,
        None,
        &mut resolve,
    );

    assert_eq!(calls, 1);
    assert!(event.emitted.is_empty());
    assert!(!event.truncated);
}

#[test]
fn lines_early_return_marks_unresolved_handles_partial() {
    let response = format!("{}\tsrc/1.rs:1-1\ncoverage: indexed 1/1\n", handle(1));
    let mut event = event();

    capture_emitted(Path::new("/repo"), &response, &mut event, None, None);

    assert!(event.emitted.is_empty());
    assert!(event.truncated);
}
