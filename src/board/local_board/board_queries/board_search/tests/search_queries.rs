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
    for query in ["missingcolumn:needle", "NEAR(", "OR NOT"] {
        let found = results(&mut board, query, None, 20);
        assert!(found["hits"].as_array().unwrap().is_empty(), "{query}");
    }
    let error = board
        .handle(&BoardRequest::new(owner(), query_op("\"", None, 20)))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    let needle = post(&mut board, plan, "needle AND thread");
    assert_eq!(
        targets(&results(&mut board, "needle AND", None, 20)),
        [needle.to_string()].into()
    );
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

#[test]
fn board_search_ignores_punctuation_only_terms() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board, "One", "body");
    let entry = post(&mut board, plan, "fix the board layout");
    assert_eq!(
        targets(&results(&mut board, "fix - board", None, 20)),
        [entry.to_string()].into()
    );
    assert_eq!(
        targets(&results(&mut board, "fix & board", None, 20)),
        [entry.to_string()].into()
    );
    integrity(&board);
}

#[test]
fn board_search_rejects_queries_without_searchable_terms() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board, "One", "body");
    post(&mut board, plan, "fix the board layout");
    for query in ["-", "&", "*", "→"] {
        let error = board
            .handle(&BoardRequest::new(owner(), query_op(query, None, 20)))
            .unwrap_err();
        assert_eq!(error.code, BoardErrorCode::InvalidOptions, "{query}");
    }
    integrity(&board);
}

#[test]
fn board_search_supports_trailing_prefix_wildcards() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board, "One", "body");
    let entry = post(&mut board, plan, "fix the board layout");
    assert_eq!(
        targets(&results(&mut board, "boa*", None, 20)),
        [entry.to_string()].into()
    );
    integrity(&board);
}

#[test]
fn board_search_finds_title_only_matches_as_plan_hits() {
    let (_directory, mut board) = database();
    let first = plan(&mut board, "Zephyr navigation overhaul", "ordinary body");
    let _other = plan(&mut board, "Unrelated title", "other body");
    // The create entry mirrors the title; neutralize it so the word is title-only.
    board
        .conn
        .execute(
            "UPDATE entries SET body='neutral create record' WHERE plan_id=?1 AND kind='create'",
            [sql_number(first.get())],
        )
        .unwrap();
    let found = results(&mut board, "zephyr", None, 20);
    assert_eq!(targets(&found), [first.to_string()].into());
    let hit = &found["hits"][0];
    assert_eq!(hit["target"], first.to_string());
    assert_eq!(hit["plan"], first.to_string());
    assert_eq!(hit["source"], "plan");
    assert_eq!(hit["snippet"], "Zephyr navigation overhaul");
    assert_eq!(found["truncated"], false);
    integrity(&board);
}

#[test]
fn board_search_indexes_plan_titles_on_insert_and_title_updates() {
    let (_directory, mut board) = database();
    let one = plan(&mut board, "One", "body one");
    // Post-migration creates are title-searchable without further action; the
    // create entry is neutralized so assertions isolate title indexing.
    let three = plan(&mut board, "Harbor pilot queue", "body three");
    board
        .conn
        .execute(
            "UPDATE entries SET body='neutral create record' WHERE plan_id=?1 AND kind='create'",
            [sql_number(three.get())],
        )
        .unwrap();
    assert_eq!(
        targets(&results(&mut board, "harbor", None, 20)),
        [three.to_string()].into()
    );
    assert!(targets(&results(&mut board, "harbor", Some(one), 20)).is_empty());
    assert_eq!(
        targets(&results(&mut board, "harbor", Some(three), 20)),
        [three.to_string()].into()
    );
    // Defensive update trigger: no production path renames plans today.
    board
        .conn
        .execute(
            "UPDATE plans SET title='Beacon pilot queue' WHERE id=?1",
            [sql_number(three.get())],
        )
        .unwrap();
    assert!(targets(&results(&mut board, "harbor", None, 20)).is_empty());
    let renamed = results(&mut board, "beacon", None, 20);
    assert_eq!(targets(&renamed), [three.to_string()].into());
    assert_eq!(renamed["hits"][0]["source"], "plan");
    assert_eq!(renamed["hits"][0]["snippet"], "Beacon pilot queue");
    integrity(&board);
}

#[test]
fn board_search_orders_mixed_title_and_body_hits_deterministically() {
    let (_directory, mut board) = database();
    // Fixed seed: a fresh database with one title hit (P1), one create-entry
    // hit (E1 mirrors the title), and one note hit (E3).
    let one = plan(&mut board, "mixword atlas", "body one");
    let two = plan(&mut board, "Two", "body two");
    let note = post(&mut board, two, "mixword entry body");
    let expected = [one.to_string(), "E1".to_owned(), note.to_string()]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let order_of = |found: &serde_json::Value| {
        found["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["target"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let first = results(&mut board, "mixword", None, 20);
    assert_eq!(targets(&first), expected);
    let order = order_of(&first);
    // Pins the UNION/ORDER BY behavior, not a relevance claim: per-table
    // bm25 is not comparable across FTS tables. The repeat run proves the
    // order is deterministic.
    assert_eq!(order, ["E1".to_owned(), note.to_string(), one.to_string()]);
    assert_eq!(order, order_of(&results(&mut board, "mixword", None, 20)));
    integrity(&board);
}

#[test]
fn board_search_matches_literal_punctuation_and_operator_words() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board, "One", "body");
    let entry = post(
        &mut board,
        plan,
        "patch board.rs for P7.3 don't notify user@host about src/x or e-mail AND NEAR",
    );
    for query in [
        "board.rs",
        "P7.3",
        "don't",
        "user@host",
        "src/x",
        "e-mail",
        "AND",
        "NEAR",
    ] {
        assert_eq!(
            targets(&results(&mut board, query, None, 20)),
            [entry.to_string()].into(),
            "{query}"
        );
    }
    integrity(&board);
}
