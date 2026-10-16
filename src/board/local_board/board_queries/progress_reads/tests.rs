use super::*;
use crate::board::board_ids::EntryId;
use crate::board::board_protocol::{GateStatus, ReviewVerdict, WorkerHandle};

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE claims(id INTEGER PRIMARY KEY,plan_id INTEGER,task_ordinal INTEGER,entry_id INTEGER);
             CREATE TABLE claim_workers(
                 id INTEGER PRIMARY KEY,claim_id INTEGER,plan_id INTEGER,task_ordinal INTEGER,
                 handle TEXT,ended_at INTEGER
             );
             CREATE TABLE progress_facts(
                 entry_id INTEGER PRIMARY KEY,claim_id INTEGER,worker_id INTEGER,
                 head_oid TEXT,head_verified INTEGER,step_done INTEGER,step_total INTEGER,
                 tests_passed INTEGER,tests_failed INTEGER,eta_at INTEGER,updated_at INTEGER
             );
             CREATE TABLE progress_gates(entry_id INTEGER,name TEXT,result TEXT);
             CREATE TABLE review_verdicts(entry_id INTEGER PRIMARY KEY,verdict TEXT,count INTEGER);
             CREATE TABLE entries(id INTEGER PRIMARY KEY,worker_id INTEGER,kind TEXT,seq INTEGER);
             INSERT INTO claims VALUES(10,1,1,101),(20,1,1,201);
             INSERT INTO claim_workers VALUES(7,10,1,1,'reviewer',99),(8,20,1,1,'reviewer',NULL);
             INSERT INTO progress_facts VALUES
                 (102,10,NULL,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',0,2,5,NULL,NULL,1100,10),
                 (103,10,NULL,NULL,NULL,NULL,NULL,4,1,1200,20),
                 (104,10,7,NULL,NULL,8,9,NULL,NULL,NULL,30),
                 (202,20,NULL,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',1,1,2,1,0,1400,40);
             INSERT INTO progress_gates VALUES(102,'cargo','pass'),(103,'cargo','fail');
             INSERT INTO entries VALUES(501,7,'review',50);
             INSERT INTO review_verdicts VALUES(501,'findings',0);",
        )
        .unwrap();
    connection
}

fn claim_entry(number: u64) -> EntryId {
    EntryId::new(number).unwrap()
}

fn worker() -> WorkerAddress {
    let task = TaskId::new(PlanId::new(1).unwrap(), 1).unwrap();
    WorkerAddress::new(task, WorkerHandle::parse("reviewer").unwrap()).unwrap()
}

#[test]
fn partial_facts_keep_source_entries_and_do_not_cross_claim_or_worker() {
    let connection = database();
    let lead_snapshot = progress_for_claim(&connection, claim_entry(101), None)
        .unwrap()
        .unwrap();
    assert_eq!(lead_snapshot.head.as_ref().unwrap().entry, claim_entry(102));
    assert_eq!(lead_snapshot.head.as_ref().unwrap().updated_at, 10);
    assert!(!lead_snapshot.head.as_ref().unwrap().value.verified);
    assert_eq!(lead_snapshot.step.as_ref().unwrap().value.current, 2);
    assert_eq!(
        lead_snapshot.tests.as_ref().unwrap().entry,
        claim_entry(103)
    );
    assert_eq!(lead_snapshot.tests.as_ref().unwrap().updated_at, 20);
    assert_eq!(lead_snapshot.eta_at.as_ref().unwrap().value, 1200);
    assert_eq!(lead_snapshot.gates.len(), 1);
    assert_eq!(lead_snapshot.gates[0].status, GateStatus::Fail);
    assert_eq!(lead_snapshot.gates[0].entry, claim_entry(103));

    let worker_snapshot = progress_for_claim(&connection, claim_entry(101), Some(&worker()))
        .unwrap()
        .unwrap();
    assert_eq!(worker_snapshot.step.as_ref().unwrap().value.current, 8);
    assert!(worker_snapshot.tests.is_none());

    let next_claim = progress_for_claim(&connection, claim_entry(201), None)
        .unwrap()
        .unwrap();
    let expected_oid = "b".repeat(40);
    assert_eq!(
        next_claim.head.as_ref().unwrap().value.oid.as_str(),
        expected_oid.as_str()
    );
    assert!(next_claim.head.as_ref().unwrap().value.verified);
    assert_eq!(next_claim.tests.as_ref().unwrap().value.passed, 1);
}

#[test]
fn entry_verdict_roundtrip_survives_worker_end() {
    let connection = database();
    assert_eq!(
        verdict_for_entry(&connection, claim_entry(501)).unwrap(),
        Some(ReviewVerdict::Findings(0))
    );
    assert_eq!(
        latest_worker_verdict(&connection, &worker()).unwrap(),
        Some(ReviewVerdict::Findings(0))
    );
}
