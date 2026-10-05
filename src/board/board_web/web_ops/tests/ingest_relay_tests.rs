use super::*;

#[test]
fn ingestion_rejects_old_api_and_another_database_before_forwarding() {
    let database = Path::new("/source/web.sqlite3");
    for status in [
        serde_json::json!({"status":"ok"}),
        serde_json::json!({"status":"ok","board_api":BOARD_API - 1,"board_db":database}),
        serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":"/source/other.sqlite3"}),
    ] {
        let mut calls = Vec::new();
        let error = ingest_via(
            database,
            QueryDeadline::after(Duration::from_secs(1)),
            &mut |args, _| {
                calls.push(args.to_vec());
                Ok(Some(status.to_string()))
            },
        )
        .unwrap_err();
        assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
        assert_eq!(
            calls,
            vec![vec![String::from("system"), String::from("status")]]
        );
    }
}

#[test]
fn ingestion_probes_then_relays_with_the_same_deadline() {
    let database = Path::new("/source/web.sqlite3");
    let mut calls = Vec::new();
    let deadline = QueryDeadline::after(Duration::from_secs(1));
    let expected_remaining = deadline.remaining();
    let reply = ingest_via(database, deadline, &mut |args, passed| {
        assert!(passed.remaining() <= expected_remaining);
        calls.push(args.to_vec());
        Ok(Some(if calls.len() == 1 {
            serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":database}).to_string()
        } else {
            serde_json::json!({"api":BOARD_API,"inserted":1}).to_string()
        }))
    })
    .unwrap();
    assert_eq!(reply["inserted"], 1);
    assert_eq!(calls[1], ["--json", "board", "ingest"]);
}

#[test]
fn ingest_flight_admits_one_relay_at_a_time() {
    let flight = IngestFlight::default();
    assert!(flight.begin().leads);
    assert!(
        !flight.begin().leads,
        "a running scan admits no second relay"
    );
    flight.finish();
    assert!(flight.begin().leads);
}

#[test]
fn mid_scan_posts_take_the_next_ticket_and_rerun_until_clean() {
    let flight = IngestFlight::default();
    let claim = flight.begin();
    assert!(claim.leads);
    let mut scans = 0;
    let mut tickets = Vec::new();
    let mut reruns = Vec::new();
    relay_flight(&flight, || {
        scans += 1;
        tickets.push(flight.current_ticket());
        // POSTs landing mid-scan take the next ticket and dirty the
        // flight; concurrent joins coalesce into a single rerun, and
        // the relay stops once a scan runs clean.
        if scans < 3 {
            let first = flight.begin();
            let second = flight.begin();
            for joined in [&first, &second] {
                assert!(!joined.leads);
                assert_ne!(joined.ticket, tickets[scans - 1]);
            }
            assert_eq!(
                first.ticket, second.ticket,
                "concurrent joins share one rerun"
            );
            reruns.push(first.ticket);
        }
    });
    assert_eq!(scans, 3, "two dirty scans rerun, the clean scan ends it");
    assert_eq!(tickets.len(), 3);
    assert_eq!(tickets[0], claim.ticket);
    assert_eq!(
        tickets[1], reruns[0],
        "the rerun runs under the next ticket"
    );
    assert_eq!(
        tickets[2], reruns[1],
        "the rerun runs under the next ticket"
    );
    let next = flight.begin();
    assert!(next.leads);
    assert_ne!(next.ticket, claim.ticket);
}

#[test]
fn post_landing_as_a_scan_completes_still_gets_a_scan() {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    let flight = IngestFlight::default();
    let leader = flight.begin();
    assert!(leader.leads);
    let gate = Arc::new(Barrier::new(2));
    let scans = Arc::new(AtomicUsize::new(0));
    let shared = &flight;
    let (first, second) = std::thread::scope(|scope| {
        let worker_gate = Arc::clone(&gate);
        let worker_scans = Arc::clone(&scans);
        scope.spawn(move || {
            relay_flight(shared, || {
                let scan = worker_scans.fetch_add(1, Ordering::SeqCst) + 1;
                // Hold the first two scans open so each POST lands
                // mid-scan, inside the completion race window.
                if scan <= 2 {
                    worker_gate.wait();
                }
            });
        });
        // Each POST lands while its scan still runs; the relay must
        // rerun once per dirty flag under the next ticket, never the
        // running scan's ticket.
        let first = flight.begin();
        gate.wait();
        let second = flight.begin();
        gate.wait();
        (first, second)
    });
    assert!(!first.leads, "a mid-scan POST never leads");
    assert!(!second.leads, "a mid-rerun POST never leads");
    assert_ne!(
        first.ticket, leader.ticket,
        "a mid-scan POST holds the next ticket, not the running scan's"
    );
    assert_ne!(
        second.ticket, first.ticket,
        "a mid-rerun POST holds the following ticket"
    );
    assert_eq!(scans.load(Ordering::SeqCst), 3, "one rerun per dirty flag");
    let next = flight.begin();
    assert!(next.leads, "the flight releases once a scan runs clean");
    assert_ne!(next.ticket, second.ticket);
}
