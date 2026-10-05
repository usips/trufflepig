use super::*;

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
