use super::*;

#[test]
fn collection_claims_preserve_duplicate_entry_ids_and_actual_actor_scope() {
    let (_directory, mut board) = database();
    for ordinal in 1..=6 {
        task(&board.conn, ordinal, 1);
    }
    for entry_id in 3..=7 {
        entry(&board.conn, entry_id, entry_id, "claim");
    }
    claim(&board.conn, 1, 1, 1, 3, 79);
    claim(&board.conn, 2, 2, 1, 3, 79);
    claim(&board.conn, 3, 3, 1, 4, 79);
    claim(&board.conn, 4, 4, 1, 5, 80);
    claim(&board.conn, 5, 5, 2, 6, 79);
    claim(&board.conn, 6, 6, 1, 7, 79);
    board
        .conn
        .execute("UPDATE claims SET ended_at=99 WHERE id=6", [])
        .unwrap();
    let repo = RepoKey::parse(&"a".repeat(40)).unwrap();
    board
        .conn
        .execute("INSERT INTO repos VALUES(?1,NULL)", [repo.as_str()])
        .unwrap();
    board
        .conn
        .execute("INSERT INTO plan_repos VALUES(1,?1)", [repo.as_str()])
        .unwrap();
    let ctx = context();
    let tx = board
        .reader
        .as_mut()
        .expect("read connection")
        .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
        .unwrap();
    let first = claim_window(&tx, &ctx, None, true, Some(&repo), false, None, None, 1).unwrap();
    assert_eq!(
        first.claims[0].cursor,
        ClaimCursor {
            entry: id(3),
            claim: 1
        }
    );
    assert_eq!(first.omitted, 2);
    task(&board.conn, 7, 8);
    entry(&board.conn, 8, 8, "claim");
    claim(&board.conn, 7, 7, 1, 8, 79);
    let second = claim_window(
        &tx,
        &ctx,
        None,
        true,
        Some(&repo),
        false,
        first.next_after,
        Some(first.through),
        1,
    )
    .unwrap();
    assert_eq!(
        second.claims[0].cursor,
        ClaimCursor {
            entry: id(3),
            claim: 2
        }
    );
    assert_eq!(second.omitted, 1);
    let third = claim_window(
        &tx,
        &ctx,
        None,
        true,
        Some(&repo),
        false,
        second.next_after,
        Some(first.through),
        1,
    )
    .unwrap();
    assert_eq!(
        third.claims[0].cursor,
        ClaimCursor {
            entry: id(4),
            claim: 3
        }
    );
    assert_eq!(third.omitted, 0);
    assert_eq!(third.next_after, None);
    tx.commit().unwrap();
    let frozen = claim_window(
        board.reader.as_ref().expect("read connection"),
        &ctx,
        None,
        true,
        Some(&repo),
        false,
        None,
        Some(first.through),
        200,
    )
    .unwrap();
    assert_eq!(frozen.claims.len(), 3);
    let public_plan = claim_window(
        board.reader.as_ref().expect("read connection"),
        &ctx,
        Some(plan(1)),
        false,
        None,
        false,
        None,
        Some(first.through),
        200,
    )
    .unwrap();
    assert_eq!(public_plan.claims.len(), 5);
    let mut desktop = context();
    desktop.actor.host = "desktop".into();
    let own = claim_window(
        board.reader.as_ref().expect("read connection"),
        &desktop,
        None,
        true,
        None,
        true,
        None,
        None,
        200,
    )
    .unwrap();
    assert_eq!(own.claims[0].claim.actor.host, "desktop");
    assert_eq!(own.claims.len(), 1);
    assert_eq!(
        claim_window(
            board.reader.as_ref().expect("read connection"),
            &ctx,
            None,
            false,
            None,
            true,
            None,
            None,
            1
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        claim_window(
            board.reader.as_ref().expect("read connection"),
            &ctx,
            None,
            true,
            None,
            true,
            Some(ClaimCursor {
                entry: id(3),
                claim: 0
            }),
            None,
            1
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
}

#[test]
fn collection_claims_show_sibling_session_stale_claims_for_resume() {
    let (_directory, board) = database();
    task(&board.conn, 1, 1);
    entry(&board.conn, 3, 3, "claim");
    board
        .conn
        .execute(
            "INSERT INTO actors VALUES(3,'josh','laptop','codex','s2')",
            [],
        )
        .unwrap();
    claim(&board.conn, 1, 1, 1, 3, 79);
    let mut second = context();
    second.actor.session = "s2".into();
    let own = claim_window(
        board.reader.as_ref().expect("read connection"),
        &second,
        None,
        true,
        None,
        true,
        None,
        None,
        200,
    )
    .unwrap();
    assert_eq!(own.claims.len(), 1);
    assert_eq!(own.claims[0].claim.task.ordinal, 1);
}
