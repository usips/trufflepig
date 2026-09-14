use super::*;
use std::io::{self, Write};

struct PartialWriter {
    limit: usize,
    bytes: Vec<u8>,
    fail_flush: bool,
}
impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let available = self.limit.saturating_sub(self.bytes.len());
        if available == 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let count = available.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::ErrorKind::Other.into())
        } else {
            Ok(())
        }
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
fn delivery_counts_partial_utf8_bytes_and_flush_failure() {
    let response = "é𝄞\n";
    let mut writer = PartialWriter {
        limit: 3,
        bytes: Vec::new(),
        fail_flush: false,
    };
    let receipt = emit_response(&mut writer, response).unwrap();
    assert_eq!(receipt.prepared_bytes, response.len());
    assert_eq!(
        receipt.prepared_tokens,
        tiktoken_rs::o200k_base()
            .unwrap()
            .encode_ordinary(response)
            .len()
    );
    assert_eq!(receipt.accepted_bytes, 3);
    assert!(!receipt.complete);
    assert_eq!(receipt.failure, Some(DeliveryFailure::BrokenPipe));
    let mut writer = PartialWriter {
        limit: 100,
        bytes: Vec::new(),
        fail_flush: true,
    };
    let receipt = emit_response(&mut writer, response).unwrap();
    assert_eq!(receipt.accepted_bytes, response.len());
    assert_eq!(receipt.failure, Some(DeliveryFailure::Flush));
    assert!(!receipt.complete);
}

#[test]
fn metadata_excludes_raw_queries_and_audit_joins_request_stages() {
    let temp = tempfile::tempdir().unwrap();
    let log = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    let mut request = event();
    request.raw_query = Some("private raw query".into());
    request.stage = EventStage::Server;
    log.record(&request).unwrap();
    request.stage = EventStage::Delivery;
    request.receipt = Some(emit_response(&mut Vec::new(), "{}\n").unwrap());
    log.record(&request).unwrap();
    let audit = log.audit(None).unwrap();
    assert_eq!(audit.retained_requests, 1);
    assert_eq!(audit.complete_deliveries, 1);
    assert_eq!(audit.incomplete_deliveries, 0);
    let content = std::fs::read_to_string(log.segments().unwrap().remove(0)).unwrap();
    assert!(!content.contains("private raw query"));
    assert!(!content.contains("raw_query"));
}

#[test]
fn incomplete_receipts_never_count_source_as_viewed() {
    let temp = tempfile::tempdir().unwrap();
    let log = DiagnosticStore::open(temp.path(), DiagnosticsMode::Detailed).unwrap();
    let mut request = event();
    request.raw_query = Some("opted in".into());
    request.emitted.push(EmittedIdentity {
        repository: "repo".into(),
        path: "lib.rs".into(),
        content_revision: "digest".into(),
        commit: None,
        blob: None,
        start_byte: 0,
        end_byte: 10,
        original_rank: Some(1),
        source_body: true,
    });
    log.record(&request).unwrap();
    let audit = log.audit(None).unwrap();
    assert_eq!(audit.viewed_source_identities, 0);
    assert_eq!(audit.incomplete_deliveries, 1);
    assert!(audit.incomplete_window);
    assert!(
        std::fs::read_to_string(log.segments().unwrap().remove(0))
            .unwrap()
            .contains("opted in")
    );
}

#[test]
fn forgetting_invalidates_open_writers_and_removes_retained_records() {
    let temp = tempfile::tempdir().unwrap();
    let old = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    let buffered = event();
    old.record(&event()).unwrap();
    let newer = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    newer.forget().unwrap();
    assert_eq!(old.record(&event()).unwrap(), RecordStatus::Invalidated);
    assert_eq!(newer.audit(None).unwrap().retained_requests, 0);
    let fresh = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    assert_eq!(fresh.record(&buffered).unwrap(), RecordStatus::Invalidated);
    assert_eq!(fresh.record(&event()).unwrap(), RecordStatus::Recorded);
}

#[test]
fn oversized_damaged_log_lines_are_bounded_and_mark_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    let log = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    log.record(&event()).unwrap();
    let path = log.segments().unwrap().remove(0);
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&vec![b'x'; 128 * 1024]).unwrap();
    file.write_all(b"\n").unwrap();
    let audit = log.audit(None).unwrap();
    assert_eq!(audit.malformed_records, 1);
    assert!(audit.incomplete_window);
}

#[test]
fn ledger_requires_matching_preimage_revision_and_overlapping_source_span() {
    let session = "explicit-session";
    let mut delivered = event();
    delivered.receipt = Some(emit_response(&mut Vec::new(), "source").unwrap());
    delivered.context.session = Some(session.into());
    let identity = EmittedIdentity {
        repository: "repo".into(),
        path: "lib.rs".into(),
        content_revision: "before".into(),
        commit: None,
        blob: None,
        start_byte: 0,
        end_byte: 10,
        original_rank: None,
        source_body: true,
    };
    delivered.emitted.push(identity.clone());
    let report = serde_json::json!({"sessions":[{"session":session,"report":{
        "changes":[{"path":"lib.rs","before_revision":"before","category":"modified",
            "source_changes":{"changes":[{"before":{"start":20,"end":30}}]}}]
    }}]});
    let ledger = super::ledger::compare(&report, &[&delivered]);
    assert_eq!(ledger.preimage_source_viewed, 1);
    assert_eq!(ledger.changed_spans_source_viewed, 0);
    delivered.emitted[0].end_byte = 25;
    assert_eq!(
        super::ledger::compare(&report, &[&delivered]).changed_spans_source_viewed,
        1
    );
    delivered.emitted[0].content_revision = "after".into();
    assert_eq!(
        super::ledger::compare(&report, &[&delivered]).preimage_source_viewed,
        0
    );
    delivered.emitted[0].content_revision = "before".into();
    delivered.context.created_unix_micros = 1000;
    delivered.receipt.as_mut().unwrap().completed_unix_micros = Some(3000);
    let mut ended_report = report;
    ended_report["sessions"][0]["report"]["after_capture_interval"] = serde_json::json!([1, 2]);
    assert_eq!(
        super::ledger::compare(&ended_report, &[&delivered]).preimage_source_viewed,
        0
    );
}

#[test]
fn queue_overflow_reports_loss_without_blocking_search() {
    let temp = tempfile::tempdir().unwrap();
    let log = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    let queue = DiagnosticQueue::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    let guard = log.lock().unwrap().unwrap();
    let started = std::time::Instant::now();
    for _ in 0..100 {
        queue.record(event());
    }
    assert!(queue.dropped() > 0);
    assert!(started.elapsed() < std::time::Duration::from_millis(100));
    drop(guard);
    drop(queue);
}

#[test]
fn default_budget_audit_keeps_counts_and_discloses_omitted_session_details() {
    let temp = tempfile::tempdir().unwrap();
    let log = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    let report = serde_json::json!({"changes":[],"details":"long retained detail ".repeat(1000)});
    log.conn.execute("INSERT INTO diagnostic_sessions(id,started,ended,status,report) VALUES('s',?1,?1,'ended',?2)", rusqlite::params![super::records::now() as i64, report.to_string()]).unwrap();
    let rendered = log
        .render_audit(&crate::output::OutputBudget::new(600).unwrap(), Some("s"))
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(value["output_truncated"], true);
    assert_eq!(value["retained_requests"], 0);
    assert!(value["ledger"]["available"].as_bool().unwrap());
    assert!(!rendered.contains("long retained detail"));
}

#[test]
fn segment_rotation_and_total_capacity_evict_oldest_before_append() {
    let temp = tempfile::tempdir().unwrap();
    let log = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    let now = super::records::now();
    for index in 0..3 {
        let path = log
            .directory
            .join(format!("events-{now:020}-{index}.jsonl"));
        let file = std::fs::File::create(path).unwrap();
        file.set_len(50 * 1024 * 1024).unwrap();
    }
    log.record(&event()).unwrap();
    let paths = log.segments().unwrap();
    assert_eq!(paths.len(), 3);
    assert!(
        !log.directory
            .join(format!("events-{now:020}-0.jsonl"))
            .exists()
    );
    assert!(
        paths
            .iter()
            .any(|path| path.metadata().unwrap().len() < 64 * 1024)
    );
    let retained: u64 = std::fs::read_dir(&log.directory)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(retained + 72 * 1024 * 1024 <= 200 * 1024 * 1024);
}

#[test]
fn lock_contention_is_bounded_and_drops_are_visible() {
    let temp = tempfile::tempdir().unwrap();
    let log = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    let locked = log.lock().unwrap().unwrap();
    let started = std::time::Instant::now();
    assert_eq!(log.record(&event()).unwrap(), RecordStatus::Dropped);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    drop(locked);
    assert_eq!(log.audit(None).unwrap().dropped_records, 1);
}

#[test]
fn private_storage_and_expired_segment_eviction_are_observable() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let log = DiagnosticStore::open(temp.path(), DiagnosticsMode::Metadata).unwrap();
    let old = log
        .directory
        .join("events-00000000000000000001-expired.jsonl");
    std::fs::write(&old, "old\n").unwrap();
    log.record(&event()).unwrap();
    assert!(!old.exists());
    assert_eq!(log.audit(None).unwrap().evicted_segments, 1);
    assert_eq!(
        log.directory.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    for path in log.segments().unwrap() {
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}
