use super::*;

#[test]
fn decide_grant_enables_proposal_decisions_until_revoked() {
    let (_directory, mut board) = database();
    let creator = actor("josh", "claude", "creator");
    let created = call(
        &mut board,
        creator.clone(),
        BoardOp::New {
            title: PlanTitle::new("Explicit authority").unwrap(),
            body: PlanText::new("original").unwrap(),
            repo_key: None,
        },
    );
    let plan = created.plan.unwrap();

    let proposal = call(
        &mut board,
        creator.clone(),
        BoardOp::Propose {
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("replacement").unwrap(),
            summary: EntryText::new("proposed change").unwrap(),
            supersedes: None,
        },
    );
    let before = snapshot(&board, plan);
    let error = board
        .handle(&BoardRequest::new(
            creator.clone(),
            BoardOp::Accept {
                proposal: proposal.entry,
                note: None,
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidActor);
    assert!(error.message.contains("lacks decide authority"));
    assert!(error.message.contains("ask owner josh to grant it"));
    assert_eq!(snapshot(&board, plan), before);

    let grant = grant_decide(&mut board, plan, "claude");
    let accepted = call(
        &mut board,
        creator.clone(),
        BoardOp::Accept {
            proposal: proposal.entry,
            note: None,
        },
    );
    assert_eq!(accepted.revision.unwrap().revision, 2);
    let authority: (String, Option<i64>) = board
        .conn
        .query_row(
            "SELECT authority_basis,authority_entry FROM entries WHERE id=?1",
            [sql_number(accepted.entry.get())],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(authority.0, "grant");
    assert_eq!(authority.1, Some(i64::try_from(grant.get()).unwrap()));

    let next_proposal = call(
        &mut board,
        creator.clone(),
        BoardOp::Propose {
            base: PlanRevision::new(plan, 2).unwrap(),
            body: PlanText::new("second replacement").unwrap(),
            summary: EntryText::new("second proposal").unwrap(),
            supersedes: None,
        },
    );
    board
        .handle(&BoardRequest::new(
            actor("josh", "human", "owner"),
            BoardOp::Revoke { entry: grant },
        ))
        .unwrap();
    let before = snapshot(&board, plan);
    let error = board
        .handle(&BoardRequest::new(
            creator,
            BoardOp::Reject {
                proposal: next_proposal.entry,
                reason: EntryText::new("revoked delegate").unwrap(),
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidActor);
    assert_eq!(snapshot(&board, plan), before);
}

#[test]
fn only_owner_human_can_create_or_revoke_plan_grants() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let error = board
        .handle(&BoardRequest::new(
            actor("other", "human", "other-owner"),
            BoardOp::Grant {
                plan: Some(plan),
                roles: vec![AuthorityRole::Decide],
                grantee: BoardRecipient::parse("claude").unwrap(),
                expires_in: None,
                note: None,
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidActor);
    let error = board
        .handle(&BoardRequest::new(
            actor("josh", "claude", "non-human-owner-session"),
            BoardOp::Grant {
                plan: Some(plan),
                roles: vec![AuthorityRole::Decide],
                grantee: BoardRecipient::parse("claude").unwrap(),
                expires_in: None,
                note: None,
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidActor);

    let grant = grant_decide(&mut board, plan, "claude");
    let error = board
        .handle(&BoardRequest::new(
            actor("other", "human", "other-owner"),
            BoardOp::Revoke { entry: grant },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidActor);
    let error = board
        .handle(&BoardRequest::new(
            actor("josh", "claude", "non-human-owner-session"),
            BoardOp::Revoke { entry: grant },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidActor);
    assert!(
        crate::board::local_board::authority_for(
            &board.conn,
            &actor("josh", "claude", "grantee"),
            Some(plan),
            AuthorityRole::Decide,
            crate::board::local_board::unix_now().unwrap(),
        )
        .unwrap()
        .is_some()
    );
    assert!(
        crate::board::local_board::board_authority::can_manage_grants(
            &board.conn,
            &actor("josh", "human", "owner"),
            plan,
        )
        .unwrap()
    );
    assert!(
        !crate::board::local_board::board_authority::can_manage_grants(
            &board.conn,
            &actor("josh", "claude", "grantee"),
            plan,
        )
        .unwrap()
    );
    board
        .handle(&BoardRequest::new(
            actor("josh", "human", "owner"),
            BoardOp::Revoke { entry: grant },
        ))
        .unwrap();
    let history =
        crate::board::local_board::board_authority::read_grants(&board.conn, Some(plan)).unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].revoked_by.is_some());
}
