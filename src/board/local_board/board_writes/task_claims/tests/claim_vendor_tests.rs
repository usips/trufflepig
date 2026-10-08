use super::*;
use crate::board::board_protocol::{AgentClaims, ReadScope};

#[test]
fn claim_record_carries_vendor() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let holder = actor("josh", "omp", "vendor");
    let plan = PlanId::new(1).unwrap();
    let mut request = BoardRequest::new(
        holder.clone(),
        BoardOp::CarveClaim {
            plan,
            title: PlanTitle::new("Vendor lane").unwrap(),
            scope: EntryText::new("vendor records").unwrap(),
            section: None,
        },
    );
    request.claims = Some(AgentClaims {
        model: Some("gpt-6.1-sol".into()),
        effort: Some("max".into()),
    });
    backend.handle(&request).unwrap();
    backend
        .handle(&BoardRequest::new(
            holder.clone(),
            BoardOp::Hello {
                model: "Kimi K2".into(),
                effort: None,
            },
        ))
        .unwrap();
    let reply = backend
        .handle(&BoardRequest::new(
            holder,
            BoardOp::Claims {
                plan: Some(plan),
                own_stale: false,
                scope: ReadScope::All,
                after: None,
                through: None,
                limit: 20,
            },
        ))
        .unwrap();
    let BoardResult::Claims(page) = reply.result else {
        panic!("expected claims");
    };
    assert_eq!(page.claims.len(), 1);
    assert_eq!(
        serde_json::to_value(&page.claims[0].claim).unwrap()["vendor"],
        "codex"
    );
    let history =
        read_claims_window(&database.connect(), plan, i64::MIN, i64::MAX, i64::MAX, 120).unwrap();
    assert_eq!(
        serde_json::to_value(&history[0]).unwrap()["vendor"],
        "codex"
    );
}

#[test]
fn delegated_vendor_records_use_each_evidence_actor() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let delegator = actor("josh", "omp", "delegator");
    let coder = actor("josh", "muse", "coder");
    for (actor, model) in [(&delegator, "Kimi K2"), (&coder, "gpt-6.1-sol")] {
        backend
            .handle(&BoardRequest::new(
                actor.clone(),
                BoardOp::Hello {
                    model: model.into(),
                    effort: Some("max".into()),
                },
            ))
            .unwrap();
    }
    let task = TaskId::new(PlanId::new(1).unwrap(), 1).unwrap();
    backend
        .handle(&BoardRequest::new(
            delegator.clone(),
            BoardOp::TaskCreate {
                plan: task.plan,
                title: PlanTitle::new("Delegated vendor lane").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    let reply = backend
        .handle(&BoardRequest::new(
            delegator.clone(),
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("coder lane").unwrap()),
                resume: ClaimResume::No,
                delegate: Some(ClaimDelegate {
                    harness: coder.harness.clone(),
                    session: coder.session.clone(),
                }),
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected delegated claim");
    };
    let conn = database.connect();
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120).unwrap();
    assert_eq!(history[0].actor, coder);
    assert_eq!(
        serde_json::to_value(&history[0]).unwrap()["vendor"],
        "codex"
    );
    let entry = crate::board::local_board::read_entry(&conn, change.entry).unwrap();
    assert_eq!(entry.actor, coder);
    assert_eq!(serde_json::to_value(&entry).unwrap()["vendor"], "codex");
    let (_, events) = backend
        .read_event_batch(EventSeq::new(change.seq.get() - 1), Some(task.plan), 20)
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].actor, delegator);
    assert_eq!(serde_json::to_value(&events[0]).unwrap()["vendor"], "kimi");
}
