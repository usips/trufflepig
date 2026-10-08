use super::*;

#[test]
fn proposal_diff_preserves_all_lines_and_the_earlier_snapshot() {
    let directory = crate::board::board_test_support::scratch("web-proposal-diff-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config.clone()),
        directory.path().join("runtime"),
    )
    .unwrap();
    let expires = Instant::now() + Duration::from_secs(5);
    let execute = |op| {
        web_ops::execute(
            &store,
            WebRequest {
                api: BOARD_API,
                op,
                project: None,
            },
            expires,
        )
    };
    let created = execute(BoardOp::New {
        title: PlanTitle::new("Plan").unwrap(),
        body: PlanText::new("base\n").unwrap(),
        steward: None,
        repo_key: None,
    })
    .unwrap();
    let BoardResult::Change(created) = created.result else {
        panic!("expected plan receipt")
    };
    let plan = created.plan.unwrap();
    let body = (0..1001)
        .map(|line| format!("row {line}\n"))
        .collect::<String>();
    let proposal = store
        .with_writer(&config, expires, |writer| {
            writer.handle(&BoardRequest::new(
                config.actor(Some("codex"), Some("worker")).unwrap(),
                BoardOp::Propose {
                    base: PlanRevision::new(plan, 1).unwrap(),
                    body: PlanText::new(body.clone()).unwrap(),
                    summary: EntryText::new("complete proposal").unwrap(),
                    supersedes: None,
                },
            ))
        })
        .unwrap();
    let BoardResult::Change(proposal) = proposal.result else {
        panic!("expected proposal receipt")
    };
    let mut earlier = None;
    let mut later = None;
    let diff = proposal_diff(&proposal.entry.to_string(), |target| {
        let reply = execute(BoardOp::Show { target })?;
        if earlier.is_none() {
            earlier = reply.snapshot_seq;
            execute(BoardOp::New {
                title: PlanTitle::new("Intervening event").unwrap(),
                body: PlanText::new("").unwrap(),
                steward: None,
                repo_key: None,
            })?;
        } else {
            later = reply.snapshot_seq;
        }
        Ok(reply)
    })
    .unwrap();
    assert!(later.unwrap() > earlier.unwrap());
    assert_eq!(diff["snapshot_seq"], serde_json::json!(earlier));
    let added = diff["hunks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|hunk| hunk["added"].as_array().unwrap())
        .map(|line| line.as_str().unwrap())
        .collect::<String>();
    assert_eq!(added, body);
    assert!(added.ends_with("row 1000\n"));
}

#[test]
fn render_plan_decodes_percent_encoded_revision_target() {
    let fixture = render_fixture();
    let plan = create_plan(&fixture);
    let revision = format!("\"revision\":\"{plan}@1\"");
    for target in [format!("{plan}@1"), format!("{plan}%401")] {
        let reply = render_get(&fixture, &format!("/api/v1/render/plan/{target}"));
        assert!(reply.starts_with("HTTP/1.1 200 "), "{target}: {reply}");
        assert!(reply.contains(&revision), "{target}: {reply}");
    }
}

#[test]
fn render_diff_decodes_percent_encoded_span_target() {
    let fixture = render_fixture();
    let plan = create_plan(&fixture);
    execute(
        &fixture,
        BoardOp::Edit {
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("base\nrevised\n").unwrap(),
            summary: EntryText::new("revise the plan").unwrap(),
        },
    );
    for target in [format!("{plan}@1..2"), format!("{plan}%401..2")] {
        let reply = render_get(&fixture, &format!("/api/v1/render/diff/{target}"));
        assert!(reply.starts_with("HTTP/1.1 200 "), "{target}: {reply}");
        assert!(reply.contains("\"hunks\":"), "{target}: {reply}");
        assert!(reply.contains("revised"), "{target}: {reply}");
    }
}

#[test]
fn render_proposal_accepts_a_literal_entry_target() {
    let fixture = render_fixture();
    let plan = create_plan(&fixture);
    let proposal = fixture
        .state
        .store
        .with_writer(
            &fixture.config,
            Instant::now() + Duration::from_secs(5),
            |writer| {
                writer.handle(&BoardRequest::new(
                    fixture.config.actor(Some("codex"), Some("worker")).unwrap(),
                    BoardOp::Propose {
                        base: PlanRevision::new(plan, 1).unwrap(),
                        body: PlanText::new("proposed\n").unwrap(),
                        summary: EntryText::new("a proposal").unwrap(),
                        supersedes: None,
                    },
                ))
            },
        )
        .unwrap();
    let BoardResult::Change(proposal) = proposal.result else {
        panic!("expected proposal receipt")
    };
    let entry = proposal.entry.to_string();
    let reply = render_get(&fixture, &format!("/api/v1/render/proposal/{entry}"));
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
    assert!(reply.contains(&format!("\"entry\":\"{entry}\"")), "{reply}");
}

#[test]
fn render_routes_reject_every_percent_escape_but_at() {
    let fixture = render_fixture();
    for path in [
        "/api/v1/render/plan/P1%4",
        "/api/v1/render/plan/P1%zz",
        "/api/v1/render/plan/P1%",
        "/api/v1/render/plan/P1%FF1",
        "/api/v1/render/diff/P1%4",
        "/api/v1/render/proposal/E%",
        "/api/v1/render/plan/P1%2f1",
        "/api/v1/render/plan/%2F",
        "/api/v1/render/plan/%00",
        "/api/v1/render/plan/P1%001",
        "/api/v1/render/plan/P1%411",
    ] {
        let reply = render_get(&fixture, path);
        assert!(reply.starts_with("HTTP/1.1 400 "), "{path}: {reply}");
        assert!(
            reply.contains("\"code\":\"invalid_options\""),
            "{path}: {reply}"
        );
    }
}
