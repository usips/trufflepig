use super::*;

#[test]
fn nested_coder_and_lead_byes_keep_cleanup_at_each_session_boundary() {
    let fixture = ByeFixture::new();
    let config = BoardConfig::for_database(&fixture.database);
    let top_lead = config
        .actor(Some("claude"), Some("top-lead-session"))
        .unwrap();
    call(
        &fixture,
        &top_lead,
        BoardOp::Hello {
            model: "opus".into(),
            effort: None,
            worker: None,
        },
    );
    let upstream_plan = new_plan(&fixture, &top_lead, "Upstream plan");
    let upstream_task = new_task(&fixture, &top_lead, upstream_plan, "Lead task");
    let upstream_claim = claim(&fixture, &top_lead, upstream_task);
    let lead_worker_address = add_worker(&fixture, &top_lead, upstream_task, "nested-lead");

    let nested_lead = config
        .actor(Some("claude"), Some("nested-lead-session"))
        .unwrap();
    call(
        &fixture,
        &nested_lead,
        BoardOp::Hello {
            model: "gpt-6-luna".into(),
            effort: Some("xhigh".into()),
            worker: Some(lead_worker_address),
        },
    );
    let nested_plan = new_plan(&fixture, &nested_lead, "Nested plan");
    let nested_task = new_task(&fixture, &nested_lead, nested_plan, "Nested lead task");
    let nested_claim = claim(&fixture, &nested_lead, nested_task);
    add_worker(&fixture, &nested_lead, nested_task, "coder");

    let coder = config.actor(Some("claude"), Some("coder-session")).unwrap();
    let coder_address =
        WorkerAddress::new(nested_task, WorkerHandle::parse("coder").unwrap()).unwrap();
    call(
        &fixture,
        &coder,
        BoardOp::Hello {
            model: "gpt-6-luna".into(),
            effort: Some("xhigh".into()),
            worker: Some(coder_address),
        },
    );

    let coder_bye = call(&fixture, &coder, BoardOp::Bye);
    assert!(matches!(
        coder_bye.result,
        BoardResult::Bye(ref bye) if bye.outcome == ByeOutcome::Ended
    ));
    let conn = rusqlite::Connection::open(&fixture.database).unwrap();
    assert_eq!(claim_end_state(&conn, upstream_claim), (None, None));
    assert_eq!(claim_end_state(&conn, nested_claim), (None, None));
    assert_eq!(
        worker_end_state(&conn, upstream_task, "nested-lead"),
        (None, None)
    );
    assert_eq!(
        worker_end_state(&conn, nested_task, "coder").1.as_deref(),
        Some("stopped")
    );

    let lead_bye = call(&fixture, &nested_lead, BoardOp::Bye);
    let BoardResult::Bye(lead_bye) = lead_bye.result else {
        panic!("expected nested lead bye");
    };
    assert_eq!(lead_bye.outcome, ByeOutcome::Ended);
    let lead_bye_entry = lead_bye.entry.expect("ended bye has an entry");
    let lead_bye_seq = lead_bye.seq.expect("ended bye has a sequence");
    assert_eq!(claim_end_state(&conn, upstream_claim), (None, None));
    assert_eq!(
        claim_end_state(&conn, nested_claim).1.as_deref(),
        Some("session_ended")
    );
    assert_eq!(
        worker_end_state(&conn, upstream_task, "nested-lead")
            .1
            .as_deref(),
        Some("stopped")
    );
    assert_eq!(
        worker_end_state(&conn, nested_task, "coder").1.as_deref(),
        Some("stopped")
    );

    let end_page = call(
        &fixture,
        &nested_lead,
        BoardOp::Entries {
            plan: Some(nested_plan),
            kind: Some(EntryKind::Claim),
            harness: None,
            user: None,
            host: None,
            task: Some(nested_task),
            references: Some(nested_claim),
            after: None,
            before: None,
            through: Some(lead_bye_seq),
            limit: 50,
        },
    );
    let BoardResult::Entries(page) = end_page.result else {
        panic!("expected nested task entries");
    };
    let end = page
        .entries
        .iter()
        .find(|entry| {
            entry.refs.contains(&BoardRef::Entry(nested_claim))
                && entry.refs.contains(&BoardRef::Entry(lead_bye_entry))
        })
        .expect("nested claim end links its start and the lead bye");
    assert_eq!(end.seq, lead_bye_seq);
    assert_eq!(end.plan, Some(nested_plan));
    assert_eq!(end.kind, EntryKind::Claim);
    assert_eq!(end.actor, nested_lead);
    assert!(end.refs.contains(&BoardRef::Task(nested_task)));

    let task_arg = nested_task.to_string();
    let resume_args: Vec<String> = ["board", "claim", &task_arg, "--resume"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let resume_error = fixture
        .host
        .run(
            &crate::board_test_support::parse_board_arguments(&resume_args).unwrap(),
            &RequestContext::new(Some("nested-lead-session".into()), Some("claude".into())),
            QueryDeadline::start(),
        )
        .unwrap_err()
        .to_string();
    assert!(
        resume_error.contains(&format!("previous claim {nested_claim}")),
        "{resume_error}"
    );
    assert!(resume_error.contains("session ended"), "{resume_error}");
    assert!(resume_error.contains("board claim"), "{resume_error}");

    let resumed_lead = nested_lead.clone();
    let fresh_claim_reply = call(
        &fixture,
        &resumed_lead,
        BoardOp::ClaimTask {
            task: nested_task,
            scope: Some(EntryText::new("fresh scope after session bye").unwrap()),
            resume: ClaimResume::No,
            delegate: None,
        },
    );
    let BoardResult::Change(fresh_claim) = fresh_claim_reply.result else {
        panic!("expected a fresh nested lead claim");
    };
    assert_ne!(fresh_claim.entry, nested_claim);

    let replacement_address = add_worker(&fixture, &top_lead, upstream_task, "nested-lead-2");
    call(
        &fixture,
        &resumed_lead,
        BoardOp::Hello {
            model: "gpt-6-luna".into(),
            effort: Some("xhigh".into()),
            worker: Some(replacement_address),
        },
    );
    let replacement_state = worker_end_state(&conn, upstream_task, "nested-lead-2");
    assert_eq!(replacement_state, (None, None));
}
