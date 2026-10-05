use super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_protocol::{
    BoardOp, BoardRequest, CommitCoauthor, CommitPlanLink, LinkedCommit,
};
use crate::identity::GitOid;

#[test]
fn collection_entries_preserve_shared_sequences_and_frozen_snapshot() {
    let (_directory, mut board) = database();
    seed_entry(&board.conn, 3, 3, 1, "note", 1, None, "first sibling");
    seed_entry(&board.conn, 4, 3, 1, "note", 1, None, "second sibling");
    seed_entry(&board.conn, 5, 4, 1, "note", 1, None, "next sequence");
    let tx = board
        .reader
        .as_mut()
        .expect("read connection")
        .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
        .unwrap();
    let first = entries(&tx, None, None, None, 2);
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(5), id(4)]
    );
    assert_eq!(
        first.next_before,
        Some(EntryCursor {
            seq: EventSeq::new(3),
            entry: id(4)
        })
    );
    assert!(first.next_after.is_none());
    assert_eq!(first.through, EventSeq::new(4));
    seed_entry(
        &board.conn,
        6,
        5,
        1,
        "note",
        1,
        None,
        "concurrent later write",
    );
    let second = entries(&tx, None, first.next_before, Some(first.through), 2);
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(3), id(1)]
    );
    assert_eq!(second.next_before, None);
    assert_eq!(max_seq(&tx).unwrap(), EventSeq::new(4));
    tx.commit().unwrap();
    let later = entries(
        board.reader.as_ref().expect("read connection"),
        None,
        second.entries.last().map(|entry| EntryCursor {
            seq: entry.seq,
            entry: entry.id,
        }),
        Some(first.through),
        2,
    );
    assert!(later.entries.is_empty());
    assert_eq!(
        max_seq(board.reader.as_ref().expect("read connection")).unwrap(),
        EventSeq::new(5)
    );
    let session: (i64, i64) = board
        .conn
        .query_row(
            "SELECT cursor_seq,last_seen FROM agent_sessions WHERE actor_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(session, (17, 1));
}

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
fn collection_entries_page_shared_sequence_commits_without_skips() {
    let (_directory, mut board) = database();
    let root = GitOid::parse("1111111111111111111111111111111111111111").unwrap();
    let repo_key = RepoKey::from_roots([root]).unwrap();
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "s1",
    )
    .unwrap();
    let commits = (0..120u32)
        .map(|number| LinkedCommit {
            repo_key: repo_key.clone(),
            oid: GitOid::parse(&format!("{number:040x}")).unwrap(),
            subject: "implement task".to_owned(),
            committed_at: 50,
            author: "Josh".to_owned(),
            coauthors: vec![CommitCoauthor {
                harness: HarnessLabel::parse("codex").unwrap(),
                model: "Model".to_owned(),
                email: "noreply@openai.com".to_owned(),
            }],
            files: 1,
            insertions: 2,
            deletions: 1,
            plans: vec![CommitPlanLink {
                plan_id: plan(1),
                task_ordinal: None,
            }],
        })
        .collect();
    let reply = board
        .handle(&BoardRequest::new(actor, BoardOp::LinkCommits { commits }))
        .unwrap();
    let BoardResult::CommitsLinked(linked) = reply.result else {
        panic!("links");
    };
    assert_eq!(linked.inserted, 120);
    assert!(linked.unknown_plans.is_empty());
    let (minimum, maximum, total): (i64, i64, i64) = board
        .conn
        .query_row(
            "SELECT min(seq),max(seq),count(*) FROM entries WHERE kind='commit'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((minimum, maximum, total), (3, 3, 120));
    let reader = board.reader.as_ref().expect("read connection");
    let mut before = None;
    let mut through = None;
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let BoardResult::Entries(page) = entries_page(
            reader,
            Some(plan(1)),
            Some(EntryKind::Commit),
            None,
            None,
            None,
            None,
            None,
            None,
            before,
            through,
            50,
        )
        .unwrap()
        .result
        else {
            panic!("entries");
        };
        assert_eq!(page.plan, Some(plan(1)));
        assert!(page.next_after.is_none());
        if through.is_none() {
            through = Some(page.through);
        }
        seen.extend(page.entries.iter().map(|entry| entry.id));
        before = page.next_before;
        pages += 1;
        if before.is_none() {
            break;
        }
    }
    assert_eq!((pages, seen.len()), (3, 120));
    let mut ordered = seen.clone();
    ordered.sort();
    ordered.dedup();
    assert_eq!(ordered.len(), 120);
    assert!(
        seen.windows(2).all(|pair| pair[0] > pair[1]),
        "newest-first within the shared sequence"
    );
}

#[test]
fn collection_entries_page_ascending_behind_an_after_cursor() {
    let (_directory, board) = database();
    seed_entry(&board.conn, 3, 3, 1, "note", 1, None, "first sibling");
    seed_entry(&board.conn, 4, 3, 1, "note", 1, None, "second sibling");
    seed_entry(&board.conn, 5, 4, 1, "note", 1, None, "next sequence");
    let reader = board.reader.as_ref().expect("read connection");
    let start = EntryCursor {
        seq: EventSeq::new(0),
        entry: id(1),
    };
    let first = entries(reader, Some(start), None, None, 2);
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(1), id(3)]
    );
    assert_eq!(first.after, Some(start));
    assert_eq!(
        first.next_after,
        Some(EntryCursor {
            seq: EventSeq::new(3),
            entry: id(3)
        })
    );
    assert!(first.next_before.is_none());
    let second = entries(reader, first.next_after, None, Some(first.through), 2);
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(4), id(5)]
    );
    assert_eq!(second.next_after, None);
    assert!(second.next_before.is_none());
}

#[test]
fn collection_entries_reject_combined_after_and_before_cursors() {
    let (_directory, board) = database();
    let cursor = EntryCursor {
        seq: EventSeq::new(1),
        entry: id(1),
    };
    assert_eq!(
        entries_page(
            board.reader.as_ref().expect("read connection"),
            Some(plan(1)),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(cursor),
            Some(cursor),
            None,
            50,
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
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
