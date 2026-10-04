use super::*;

#[test]
fn collection_attention_filters_current_proposal_authority_before_limit_and_count() {
    let (_directory, board) = database();
    seed_entry(
        &board.conn,
        3,
        3,
        1,
        "proposal",
        1,
        None,
        "own current proposal",
    );
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "proposal",
        3,
        None,
        "foreign current proposal",
    );
    seed_entry(
        &board.conn,
        5,
        5,
        1,
        "question",
        3,
        None,
        "applicable question",
    );
    seed_entry(
        &board.conn,
        6,
        6,
        1,
        "proposal",
        3,
        None,
        "already accepted",
    );
    seed_entry(
        &board.conn,
        7,
        7,
        2,
        "proposal",
        1,
        None,
        "another owner's plan",
    );
    board.conn.execute_batch(
        "INSERT INTO proposals VALUES(3,1,1,'one','open',NULL,NULL),(4,1,1,'one','open',NULL,NULL),(6,1,1,'one','accepted',NULL,NULL),(7,2,1,'one','open',NULL,NULL); UPDATE plans SET owner_user='other' WHERE id=2;"
    ).unwrap();
    let ordinary = attention_page(
        board.reader.as_ref().expect("read connection"),
        &context(),
        None,
        true,
        1,
    );
    assert_eq!(attention_ids(&ordinary), vec![id(5)]);
    assert_eq!(ordinary.entries_omitted, 0);
    assert_eq!(ordinary.next_after, None);
    assert!(ordinary.rebase_needed.is_empty());
    let human = authority_actor("josh", "laptop", "human", "h1");
    let decision = attention_page(
        board.reader.as_ref().expect("read connection"),
        &human,
        None,
        true,
        1,
    );
    assert_eq!(attention_ids(&decision), vec![id(3)]);
    assert_eq!(decision.entries_omitted, 2);
    let BoardResult::Attention(rest) = attention(
        board.reader.as_ref().expect("read connection"),
        &human,
        None,
        true,
        decision.next_after,
        Some(decision.through),
        200,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(attention_ids(&rest), vec![id(4), id(5)]);
    let steward = authority_actor("josh", "laptop", "claude", "s2");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &steward,
            None,
            true,
            200
        )),
        vec![id(3), id(4), id(5)]
    );
    let foreign_human = authority_actor("other", "laptop", "human", "h1");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &foreign_human,
            None,
            true,
            200
        )),
        vec![id(5), id(7)]
    );
    let foreign_steward = authority_actor("other", "laptop", "claude", "s2");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &foreign_steward,
            None,
            true,
            200
        )),
        vec![id(5)]
    );
}

#[test]
fn collection_attention_stale_proposals_require_exact_author_for_rebase() {
    let (_directory, board) = database();
    seed_entry(
        &board.conn,
        3,
        3,
        1,
        "proposal",
        1,
        Some("muse"),
        "own stale proposal",
    );
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "proposal",
        3,
        None,
        "foreign stale proposal",
    );
    seed_entry(&board.conn, 5, 5, 1, "question", 3, None, "question");
    board.conn.execute_batch("INSERT INTO proposals VALUES(3,1,1,'one','open',NULL,NULL),(4,1,1,'one','open',NULL,NULL); UPDATE plans SET head_revision=2 WHERE id=1;").unwrap();
    let own = attention_page(
        board.reader.as_ref().expect("read connection"),
        &context(),
        None,
        false,
        1,
    );
    assert_eq!(attention_ids(&own), vec![id(3)]);
    assert_eq!(own.rebase_needed, vec![id(3)]);
    assert_eq!(
        own.entries_omitted, 1,
        "the unlinked plan's unaddressed question is global in scoped attention"
    );
    let human = authority_actor("josh", "laptop", "human", "h1");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &human,
            None,
            true,
            200
        )),
        vec![id(5)]
    );
    let steward = authority_actor("josh", "laptop", "claude", "s2");
    let own = attention_page(
        board.reader.as_ref().expect("read connection"),
        &steward,
        None,
        true,
        200,
    );
    assert_eq!(attention_ids(&own), vec![id(4), id(5)]);
    assert_eq!(own.rebase_needed, vec![id(4)]);
    let other_session = authority_actor("josh", "laptop", "claude", "s3");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &other_session,
            None,
            true,
            200
        )),
        vec![id(5)]
    );
    let other_host = authority_actor("josh", "desktop", "codex", "s1");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &other_host,
            None,
            true,
            200
        )),
        vec![id(5)]
    );
}

#[test]
fn collection_attention_scoped_shows_authority_decisions_without_repo_scope() {
    let (_directory, board) = database();
    seed_entry(
        &board.conn,
        3,
        3,
        1,
        "proposal",
        4,
        None,
        "current proposal awaiting decision",
    );
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "feedback",
        4,
        None,
        "open feedback on owned plan",
    );
    seed_entry(
        &board.conn,
        5,
        5,
        1,
        "question",
        4,
        None,
        "unaddressed question",
    );
    board.conn.execute_batch(
        "INSERT INTO proposals VALUES(3,1,1,'one','open',NULL,NULL); UPDATE entries SET state='open' WHERE id=4;"
    ).unwrap();
    let human = authority_actor("josh", "laptop", "human", "h1");
    let scoped = attention_page(
        board.reader.as_ref().expect("read connection"),
        &human,
        None,
        false,
        200,
    );
    assert_eq!(
        attention_ids(&scoped),
        vec![id(3), id(4), id(5)],
        "an unaddressed question on an unlinked plan is global in scoped attention"
    );
    assert_eq!(scoped.entries_omitted, 0);
    assert_eq!(scoped.next_after, None);
    let steward = authority_actor("josh", "laptop", "claude", "s2");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &steward,
            None,
            false,
            200
        )),
        vec![id(3), id(4), id(5)]
    );
    for actor in [human, steward] {
        assert_eq!(
            attention_ids(&attention_page(
                board.reader.as_ref().expect("read connection"),
                &actor,
                None,
                true,
                200
            )),
            vec![id(3), id(4), id(5)]
        );
    }
}

#[test]
fn collection_attention_scoped_hides_authority_decisions_from_foreign_actors() {
    let (_directory, board) = database();
    seed_entry(
        &board.conn,
        3,
        3,
        1,
        "proposal",
        4,
        None,
        "current proposal awaiting decision",
    );
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "feedback",
        4,
        None,
        "open feedback on owned plan",
    );
    seed_entry(
        &board.conn,
        5,
        5,
        1,
        "question",
        4,
        None,
        "unaddressed question",
    );
    board.conn.execute_batch(
        "INSERT INTO proposals VALUES(3,1,1,'one','open',NULL,NULL); UPDATE entries SET state='open' WHERE id=4;"
    ).unwrap();
    let foreign_human = authority_actor("other", "laptop", "human", "h1");
    let foreign_steward = authority_actor("other", "laptop", "claude", "s2");
    for actor in [&foreign_human, &foreign_steward] {
        assert_eq!(
            attention_ids(&attention_page(
                board.reader.as_ref().expect("read connection"),
                actor,
                None,
                false,
                200
            )),
            vec![id(5)],
            "authority decisions stay hidden, but an unlinked plan's question is global"
        );
    }
    for actor in [foreign_human, foreign_steward] {
        assert_eq!(
            attention_ids(&attention_page(
                board.reader.as_ref().expect("read connection"),
                &actor,
                None,
                true,
                200
            )),
            vec![id(5)]
        );
    }
}

#[test]
fn collection_attention_scope_treats_unlinked_plans_as_global_without_leaks() {
    let (_directory, board) = database();
    let repo_a = RepoKey::parse(&"a".repeat(40)).unwrap();
    let repo_b = RepoKey::parse(&"b".repeat(40)).unwrap();
    board
        .conn
        .execute(
            "INSERT INTO repos VALUES(?1,NULL),(?2,NULL)",
            params![repo_a.as_str(), repo_b.as_str()],
        )
        .unwrap();
    board
        .conn
        .execute("INSERT INTO plan_repos VALUES(2,?1)", [repo_b.as_str()])
        .unwrap();
    seed_entry(
        &board.conn,
        3,
        3,
        1,
        "question",
        4,
        None,
        "question on an unlinked plan",
    );
    seed_entry(
        &board.conn,
        4,
        4,
        2,
        "question",
        4,
        None,
        "question on another repository's plan",
    );
    board.conn.execute_batch(
        "INSERT INTO tasks VALUES(1,1,'Task A','todo',NULL,NULL,1),(2,1,'Task B','todo',NULL,NULL,1);
         INSERT INTO claims(plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active) VALUES(1,1,1,1,'scope',10,0),(2,1,1,1,'scope',10,0);"
    ).unwrap();
    let scoped = attention_page(
        board.reader.as_ref().expect("read connection"),
        &context(),
        Some(&repo_a),
        false,
        200,
    );
    assert_eq!(
        attention_ids(&scoped),
        vec![id(3)],
        "an unlinked plan is global; another repository's plan must not leak"
    );
    assert_eq!(
        scoped
            .stale_claims
            .iter()
            .map(|view| view.claim.task)
            .collect::<Vec<_>>(),
        vec![TaskId::new(plan(1), 1).unwrap()],
        "stale claims follow the same global-or-linked scope"
    );
    let global = attention_page(
        board.reader.as_ref().expect("read connection"),
        &context(),
        Some(&repo_a),
        true,
        200,
    );
    assert_eq!(attention_ids(&global), vec![id(3), id(4)]);
    assert_eq!(global.stale_claims.len(), 2);
}
