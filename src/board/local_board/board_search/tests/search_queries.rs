use super::*;

#[test]
fn board_search_preserves_shared_bodies_and_unambiguous_entry_revision_targets() {
    let (_directory, mut board) = database();
    let one = plan(&mut board, "One", "sharedneedle repeated immutable body");
    let two = plan(&mut board, "Two", "sharedneedle repeated immutable body");
    call(
        &mut board,
        BoardOp::Edit {
            base: PlanRevision::new(one, 1).unwrap(),
            body: PlanText::new("sharedneedle repeated immutable body").unwrap(),
            summary: EntryText::new("copy revision").unwrap(),
        },
    );
    let proposal = call(
        &mut board,
        BoardOp::Propose {
            base: PlanRevision::new(one, 2).unwrap(),
            body: PlanText::new("sharedneedle uniquepayload proposal body").unwrap(),
            summary: EntryText::new("proposal summary").unwrap(),
            supersedes: None,
        },
    );
    let note = post(&mut board, two, "sharedneedle entry body");
    board
        .conn
        .execute(
            "INSERT INTO texts(hash,body) VALUES('unreferenced','sharedneedle orphansecret')",
            [],
        )
        .unwrap();
    let found = results(&mut board, "sharedneedle", None, 50);
    assert_eq!(
        targets(&found),
        [
            "P1@1".to_owned(),
            "P1@2".to_owned(),
            "P2@1".to_owned(),
            proposal.entry.to_string(),
            note.to_string()
        ]
        .into()
    );
    assert_eq!(found["truncated"], false);
    let proposal_hit = &results(&mut board, "uniquepayload", None, 50)["hits"][0];
    assert_eq!(proposal_hit["target"], proposal.entry.to_string());
    assert_eq!(proposal_hit["source"], "proposal");
    assert!(
        proposal_hit["snippet"]
            .as_str()
            .unwrap()
            .contains("uniquepayload")
    );
    assert!(targets(&results(&mut board, "orphansecret", None, 50)).is_empty());
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT count(*) FROM search_documents WHERE target IN ('E1','P1@1')",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    integrity(&board);
}

#[test]
fn board_search_filters_plan_before_limit_and_reports_truncation() {
    let (_directory, mut board) = database();
    let one = plan(&mut board, "One", "first body");
    for index in 0..55 {
        post(&mut board, one, &format!("scopeword item {index}"));
    }
    let two = plan(&mut board, "Two", "second body");
    let first = post(&mut board, two, "scopeword item first");
    post(&mut board, two, "scopeword item second");
    let scoped = results(&mut board, "scopeword", Some(two), 1);
    assert_eq!(targets(&scoped), [first.to_string()].into());
    assert_eq!(scoped["hits"][0]["plan"], two.to_string());
    assert_eq!(scoped["truncated"], true);
    let all = results(&mut board, "scopeword", None, 50);
    assert_eq!(all["hits"].as_array().unwrap().len(), 50);
    assert_eq!(all["truncated"], true);
}

#[test]
fn board_search_returns_typed_query_errors_and_bounded_plain_snippets() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board, "One", "first body");
    for query in ["\"", "needle AND", "missingcolumn:needle", "NEAR("] {
        let error = board
            .handle(&BoardRequest::new(owner(), query_op(query, None, 20)))
            .unwrap_err();
        assert_eq!(
            error.code,
            BoardErrorCode::InvalidOptions,
            "{query}: {error}"
        );
    }
    for (query, limit) in [
        (" ".to_owned(), 20),
        ("x".repeat(4097), 20),
        ("x\0y".to_owned(), 20),
        ("needle".to_owned(), 0),
        ("needle".to_owned(), 51),
    ] {
        let error = board
            .handle(&BoardRequest::new(owner(), query_op(&query, None, limit)))
            .unwrap_err();
        assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    }
    let error = board
        .handle(&BoardRequest::new(
            owner(),
            query_op("needle", Some(PlanId::new(999).unwrap()), 20),
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidReference);
    let body = format!(
        "snippetneedle <img src=x onerror=alert(1)> {}",
        "é".repeat(1700)
    );
    let entry = post(&mut board, plan, &body);
    let result = results(&mut board, "snippetneedle", Some(plan), 20);
    assert_eq!(targets(&result), [entry.to_string()].into());
    let snippet = result["hits"][0]["snippet"].as_str().unwrap();
    assert!(snippet.len() <= 512);
    assert!(snippet.contains("<img src=x onerror=alert(1)>"));
    assert!(snippet.contains("snippetneedle"));
    integrity(&board);
}

#[test]
fn board_search_uses_a_query_only_committed_snapshot_without_actor_writes() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board, "One", "committedneedle body");
    let snapshot = board.max_seq().unwrap();
    board.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    board.conn.execute(
        "INSERT INTO entries(plan_id,kind,body,actor_id,seq,created_at) VALUES(?1,'note','draftneedle body',1,2,1)",
        [sql_number(plan.get())],
    ).unwrap();
    let before = board.conn.total_changes();
    let newcomer = BoardActor::new(
        "new-user",
        "host",
        HarnessLabel::parse("codex").unwrap(),
        "never-written",
    )
    .unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            newcomer.clone(),
            query_op("committedneedle", Some(plan), 20),
        ))
        .unwrap();
    assert_eq!(reply.snapshot_seq, Some(snapshot));
    let visible = serde_json::to_value(reply.result).unwrap();
    assert_eq!(targets(&visible["data"]), ["P1@1".to_owned()].into());
    let draft = board
        .handle(&BoardRequest::new(
            newcomer,
            query_op("draftneedle", None, 20),
        ))
        .unwrap();
    let visible = serde_json::to_value(draft.result).unwrap();
    assert!(targets(&visible["data"]).is_empty());
    assert_eq!(draft.snapshot_seq, Some(snapshot));
    assert_eq!(board.conn.total_changes(), before);
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM actors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        board
            .reader
            .as_ref()
            .unwrap()
            .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    board.conn.execute_batch("ROLLBACK").unwrap();
    integrity(&board);
}
