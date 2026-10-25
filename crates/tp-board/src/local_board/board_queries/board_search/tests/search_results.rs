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
