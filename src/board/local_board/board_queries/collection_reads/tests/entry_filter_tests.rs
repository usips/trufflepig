use super::*;

#[test]
fn collection_entry_filters_match_all_actor_fields_and_task_links() {
    let (_directory, board) = database();
    seed_task(&board.conn, 1);
    seed_entry(&board.conn, 3, 3, 1, "note", 1, None, "matching");
    seed_entry(&board.conn, 4, 4, 1, "note", 2, None, "different host");
    seed_entry(&board.conn, 5, 5, 1, "note", 4, None, "different user");
    seed_entry(&board.conn, 6, 6, 1, "note", 3, None, "different harness");
    seed_entry(&board.conn, 7, 7, 1, "progress", 1, None, "different kind");
    seed_entry(&board.conn, 8, 8, 1, "note", 1, None, "different task");
    board
        .conn
        .execute_batch(
            "INSERT INTO entry_refs VALUES(3,'P1.1'),(4,'P1.1'),(5,'P1.1'),(6,'P1.1'),(7,'P1.1');",
        )
        .unwrap();
    let harness = HarnessLabel::parse("codex").unwrap();
    let task = TaskId::new(plan(1), 1).unwrap();
    let BoardResult::Entries(page) = entries_page(
        board.reader.as_ref().expect("read connection"),
        Some(plan(1)),
        Some(EntryKind::Note),
        Some(&harness),
        Some("josh"),
        Some("laptop"),
        Some(task),
        None,
        None,
        None,
        None,
        200,
    )
    .unwrap()
    .result
    else {
        panic!("entries");
    };
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(3)]
    );
    let repo = register_repositories(&board.conn);
    seed_entry(&board.conn, 9, 9, 1, "commit", 1, None, "linked commit");
    let oid = "c".repeat(40);
    board
        .conn
        .execute(
            concat!(
                "INSERT INTO commits(repo_key,oid,subject,committed_at,author,coauthors,files,insertions,",
                "deletions) VALUES(?1,?2,'subject',1,'author','[]',1,1,1)"
            ),
            params![repo.as_str(), oid],
        )
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO commit_plans(repo_key,oid,plan_id,entry_id) VALUES(?1,?2,1,9)",
            params![repo.as_str(), oid],
        )
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO commit_tasks(repo_key,oid,plan_id,task_ordinal) VALUES(?1,?2,1,1)",
            params![repo.as_str(), oid],
        )
        .unwrap();
    let BoardResult::Entries(page) = entries_page(
        board.reader.as_ref().expect("read connection"),
        None,
        Some(EntryKind::Commit),
        None,
        None,
        None,
        Some(task),
        None,
        None,
        None,
        None,
        200,
    )
    .unwrap()
    .result
    else {
        panic!("entries");
    };
    assert_eq!(page.entries[0].id, id(9));
    assert_eq!(
        entries_page(
            board.reader.as_ref().expect("read connection"),
            Some(plan(2)),
            None,
            None,
            None,
            None,
            Some(task),
            None,
            None,
            None,
            None,
            200
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
}

#[test]
fn collection_entry_pages_stay_within_their_plan() {
    let (_directory, board) = database();
    board
        .conn
        .execute(
            "INSERT INTO plans(id,title,owner_user,steward,head_revision,created_at) \
             VALUES(3,'Three','josh',NULL,1,1)",
            [],
        )
        .unwrap();
    for number in 0..30u64 {
        seed_entry(
            &board.conn,
            10 + number * 2,
            10 + number * 2,
            1,
            "note",
            1,
            None,
            "plan one",
        );
        seed_entry(
            &board.conn,
            11 + number * 2,
            11 + number * 2,
            3,
            "note",
            1,
            None,
            "plan three",
        );
    }
    let reader = board.reader.as_ref().expect("read connection");
    let mut before = None;
    let mut through = None;
    let mut seen = Vec::new();
    loop {
        let BoardResult::Entries(page) = entries_page(
            reader,
            Some(plan(3)),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            before,
            through,
            20,
        )
        .unwrap()
        .result
        else {
            panic!("entries");
        };
        assert_eq!(page.plan, Some(plan(3)));
        assert!(page.entries.iter().all(|entry| entry.plan == Some(plan(3))));
        if through.is_none() {
            through = Some(page.through);
        }
        seen.extend(page.entries.iter().map(|entry| entry.id));
        before = page.next_before;
        if before.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 30);
    assert!(
        seen.windows(2).all(|pair| pair[0] > pair[1]),
        "newest-first within the plan"
    );
}
