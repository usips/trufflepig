use super::*;

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
