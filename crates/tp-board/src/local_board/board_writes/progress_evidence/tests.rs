use super::*;
use crate::board_actor::BoardActor;
use crate::board_ids::EventSeq;
use crate::board_protocol::{ProgressStep, ProgressTests};
use tp_core::request_context::HarnessLabel;

fn write_context(now: i64) -> WriteContext {
    WriteContext {
        actor_id: 1,
        actor: BoardActor::new(
            "fixture",
            "fixture-host",
            HarnessLabel::parse("codex").unwrap(),
            "progress-tests",
        )
        .unwrap(),
        model: None,
        effort: None,
        now,
        seq: EventSeq::new(1),
        claim_ttl_secs: 120,
        via: None,
        origin: None,
        authority: None,
    }
}

fn input(head: Option<GitOid>, step: Option<ProgressStep>) -> ProgressInput {
    ProgressInput {
        head,
        step,
        tests: Some(ProgressTests {
            passed: 2,
            failed: 0,
        }),
        gates: Vec::new(),
        eta_secs: None,
    }
}

#[test]
fn short_heads_and_inverted_steps_are_rejected() {
    assert!(GitOid::parse(&"a".repeat(39)).is_err());

    let context = write_context(100);
    let inverted = input(
        None,
        Some(ProgressStep {
            current: 4,
            total: 3,
        }),
    );
    assert!(validation::validate_progress(&inverted, None, &context).is_err());
}

#[test]
fn an_unresolved_head_is_kept_unverified_and_trusted_mismatch_fails() {
    let context = write_context(100);
    let reported = GitOid::parse(&"a".repeat(40)).unwrap();
    let unverified =
        validation::validate_progress(&input(Some(reported), None), None, &context).unwrap();
    assert_eq!(unverified.head_oid.as_deref(), Some(reported.as_str()));
    assert_eq!(unverified.head_verified, Some(0));

    let verified =
        validation::validate_progress(&input(Some(reported), None), Some(&reported), &context)
            .unwrap();
    assert_eq!(verified.head_verified, Some(1));

    let different = GitOid::parse(&"b".repeat(40)).unwrap();
    assert!(
        validation::validate_progress(&input(Some(reported), None), Some(&different), &context,)
            .is_err()
    );
}

#[test]
fn an_ended_worker_cannot_write_progress() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE claim_workers(
                 id INTEGER PRIMARY KEY,claim_id INTEGER,actor_id INTEGER,ended_at INTEGER,
                 plan_id INTEGER,task_ordinal INTEGER,handle TEXT
             );
             INSERT INTO claim_workers VALUES(7,12,1,90,1,1,'reviewer');",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    let task = TaskId::new(crate::board_ids::PlanId::new(1).unwrap(), 1).unwrap();
    assert!(
        authorization::active_worker_id(&transaction, &write_context(100), task, 12, "reviewer")
            .is_err()
    );
}
