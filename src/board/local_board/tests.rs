use super::*;
use crate::board::board_vocabulary::{EntryText, PlanText, PlanTitle};

fn actor(harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn database() -> (LocalBoard, PathBuf) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/board-storage-tests")
        .join(uuid::Uuid::new_v4().to_string())
        .join("board.sqlite3");
    (
        LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap(),
        path,
    )
}

fn new_plan(board: &mut LocalBoard, author: BoardActor, title: &str) -> BoardChange {
    let reply = board
        .handle(&BoardRequest::new(
            author,
            BoardOp::New {
                title: PlanTitle::new(title).unwrap(),
                body: PlanText::new("# Scope\noriginal").unwrap(),
                steward: Some(HarnessLabel::parse("claude").unwrap()),
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("new plan result");
    };
    change
}

#[test]
fn local_board_immutable_revisions_cas_and_owner_authority() {
    let (mut board, path) = database();
    let created = new_plan(&mut board, actor("human", "h1"), "Trial");
    let plan = created.plan.unwrap();
    assert_eq!(created.revision.unwrap().to_string(), "P1@1");
    let base = crate::board::board_ids::PlanRevision::new(plan, 1).unwrap();
    let proposed = board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::Propose {
                supersedes: None,
                base,
                body: PlanText::new("accepted body").unwrap(),
                summary: EntryText::new("proposed").unwrap(),
            },
        ))
        .unwrap();
    let BoardResult::Change(proposed) = proposed.result else {
        panic!("proposal result");
    };
    let unauthorized = board
        .handle(&BoardRequest::new(
            actor("muse", "m1"),
            BoardOp::Accept {
                proposal: proposed.entry,
                note: None,
            },
        ))
        .unwrap_err();
    assert_eq!(unauthorized.code, BoardErrorCode::InvalidActor);
    let accepted = board
        .handle(&BoardRequest::new(
            actor("claude", "a1"),
            BoardOp::Accept {
                proposal: proposed.entry,
                note: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(accepted) = accepted.result else {
        panic!("accept result");
    };
    assert_eq!(accepted.revision.unwrap().revision, 2);
    let stale = board
        .handle(&BoardRequest::new(
            actor("human", "h1"),
            BoardOp::Edit {
                base,
                body: PlanText::new("stale").unwrap(),
                summary: EntryText::new("stale edit").unwrap(),
            },
        ))
        .unwrap_err();
    assert_eq!(stale.code, BoardErrorCode::StaleRevision);
    let result = board
        .handle(&BoardRequest::new(
            actor("human", "h1"),
            BoardOp::Show {
                target: Some(BoardRef::Revision(base)),
            },
        ))
        .unwrap();
    let BoardResult::Revision(revision) = result.result else {
        panic!("revision result");
    };
    assert_eq!(revision.body.as_str(), "# Scope\noriginal");
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn local_board_dedupe_distinguishes_target_and_snapshots_claims() {
    let (mut board, path) = database();
    let author = actor("codex", "c1");
    let p1 = new_plan(&mut board, actor("human", "h1"), "One")
        .plan
        .unwrap();
    let p2 = new_plan(&mut board, actor("human", "h1"), "Two")
        .plan
        .unwrap();
    let op = |plan| BoardOp::Post {
        target: BoardRef::Plan(plan),
        kind: EntryKind::Progress,
        body: EntryText::new("same body").unwrap(),
        to: None,
        supersedes: None,
    };
    let mut first_request = BoardRequest::new(author.clone(), op(p1));
    first_request.claims = Some(AgentClaims {
        model: Some("test-model".to_owned()),
        effort: Some("xhigh".to_owned()),
    });
    let first = board.handle(&first_request).unwrap();
    let repeat = board.handle(&first_request).unwrap();
    let second = board.handle(&BoardRequest::new(author, op(p2))).unwrap();
    let BoardResult::Change(first) = first.result else {
        panic!("first");
    };
    let BoardResult::Change(repeat) = repeat.result else {
        panic!("repeat");
    };
    let BoardResult::Change(second) = second.result else {
        panic!("second");
    };
    assert!(repeat.deduplicated);
    assert_eq!(repeat.entry, first.entry);
    assert_ne!(second.entry, first.entry);
    assert_eq!(
        read_entry(&board.conn, first.entry)
            .unwrap()
            .model
            .as_deref(),
        Some("test-model")
    );
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn local_board_refuses_newer_schema_without_changing_journal() {
    let (board, path) = database();
    board.conn.pragma_update(None, "user_version", 99).unwrap();
    board
        .conn
        .pragma_update(None, "journal_mode", "DELETE")
        .unwrap();
    drop(board);
    let error = match LocalBoard::open_path(&path, Duration::from_secs(120)) {
        Err(error) => error,
        Ok(_) => panic!("accepted newer schema"),
    };
    assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
    let conn = Connection::open(&path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    drop(conn);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn local_board_concurrent_first_open_serializes_migration() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/board-storage-tests")
        .join(uuid::Uuid::new_v4().to_string())
        .join("board.sqlite3");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|index| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
                new_plan(
                    &mut board,
                    actor("human", &format!("h{index}")),
                    &format!("Plan {index}"),
                )
                .plan
                .unwrap()
            })
        })
        .collect();
    let ids: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_ne!(ids[0], ids[1]);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn local_board_lazy_open_respects_the_supplied_lock_timeout() {
    let (board, path) = database();
    board.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let error = match LocalBoard::open_path_with_timeout(
        &path,
        Duration::from_secs(120),
        Duration::from_millis(20),
    ) {
        Err(error) => error,
        Ok(_) => panic!("opened a locked database"),
    };
    assert_eq!(error.code, BoardErrorCode::DatabaseLocked);
    assert!(started.elapsed() < Duration::from_millis(500));
    board.conn.execute_batch("ROLLBACK").unwrap();
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn local_board_preserves_readonly_directory_and_private_file_modes() {
    use std::os::unix::fs::PermissionsExt;
    let (mut board, path) = database();
    let created = new_plan(&mut board, actor("human", "h1"), "Trial");
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    let parent = path.parent().unwrap();
    assert_eq!(
        parent.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failure = board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::Post {
                target: BoardRef::Plan(created.plan.unwrap()),
                kind: EntryKind::Note,
                body: EntryText::new("blocked write").unwrap(),
                to: None,
                supersedes: None,
            },
        ))
        .unwrap_err();
    assert_eq!(failure.code, BoardErrorCode::BoardUnavailable);
    assert!(LocalBoard::open_path(&path, Duration::from_secs(120)).is_err());
    assert_eq!(
        parent.metadata().unwrap().permissions().mode() & 0o777,
        0o500
    );
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(board);
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn local_board_commit_batch_is_atomic_idempotent_and_emits_one_event() {
    let (mut board, path) = database();
    let author = actor("human", "h1");
    let plan = new_plan(&mut board, author.clone(), "Trial").plan.unwrap();
    let root = crate::identity::GitOid::parse("1111111111111111111111111111111111111111").unwrap();
    let repo_key = RepoKey::from_roots([root]).unwrap();
    let commit = |oid: &str| LinkedCommit {
        repo_key: repo_key.clone(),
        oid: crate::identity::GitOid::parse(oid).unwrap(),
        subject: "implement task".to_owned(),
        committed_at: unix_now().unwrap(),
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
            plan_id: plan,
            task_ordinal: None,
        }],
    };
    let request = BoardRequest::new(
        author,
        BoardOp::LinkCommits {
            commits: vec![
                commit("2222222222222222222222222222222222222222"),
                commit("3333333333333333333333333333333333333333"),
            ],
        },
    );
    let before = board.max_seq().unwrap().get();
    let linked = board.handle(&request).unwrap();
    let BoardResult::CommitsLinked(linked) = linked.result else {
        panic!("links");
    };
    assert_eq!(linked.inserted, 2);
    assert_eq!(board.max_seq().unwrap().get(), before + 1);
    let linked = board.handle(&request).unwrap();
    let BoardResult::CommitsLinked(linked) = linked.result else {
        panic!("links");
    };
    assert_eq!(linked.inserted, 0);
    assert_eq!(board.max_seq().unwrap().get(), before + 1);
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn show_uses_read_connection_while_writer_transaction_is_held() {
    let (mut board, path) = database();
    let plan = new_plan(&mut board, actor("human", "h1"), "Read during write")
        .plan
        .unwrap();
    board.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "new-reader"),
            BoardOp::Show {
                target: Some(BoardRef::Plan(plan)),
            },
        ))
        .unwrap();
    board.conn.execute_batch("ROLLBACK").unwrap();
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn readonly_dispatch_leaves_actors_sessions_and_claims_untouched() {
    let (mut board, path) = database();
    let created = new_plan(&mut board, actor("human", "h1"), "Read evidence");
    let plan = created.plan.unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new("Lane").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::ClaimTask {
                task: super::super::board_ids::TaskId::new(plan, 1).unwrap(),
                scope: Some(EntryText::new("scope").unwrap()),
                resume: false,
            },
        ))
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE agent_sessions SET last_seen=7,model='stored-model',effort='stored-effort'",
            [],
        )
        .unwrap();
    board
        .conn
        .execute("UPDATE claims SET last_active=9", [])
        .unwrap();
    let before = board.conn.total_changes();
    board.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut request = BoardRequest::new(
        actor("codex", "c1"),
        BoardOp::Show {
            target: Some(BoardRef::Plan(plan)),
        },
    );
    request.claims = Some(AgentClaims {
        model: Some("replacement".into()),
        effort: Some("replacement".into()),
    });
    board.handle(&request).unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::Inbox {
                after: Some(EventSeq::new(0)),
                limit: 20,
            },
        ))
        .unwrap();
    board
        .handle(&BoardRequest::new(
            actor("new-harness", "never-written"),
            BoardOp::Show { target: None },
        ))
        .unwrap();
    assert_eq!(board.conn.total_changes(), before);
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM actors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(board.conn.query_row("SELECT count(*) FROM agent_sessions WHERE last_seen<>7 OR model<>'stored-model' OR effort<>'stored-effort'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(
        board
            .conn
            .query_row("SELECT last_active FROM claims", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        board
            .reader
            .as_ref()
            .unwrap()
            .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
            .unwrap(),
        true
    );
    board.conn.execute_batch("ROLLBACK").unwrap();
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn readonly_open_answers_all_read_kinds_without_write_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (mut writer, path) = database();
    let plan = new_plan(&mut writer, actor("human", "h1"), "Readonly evidence")
        .plan
        .unwrap();
    writer
        .conn
        .execute("UPDATE agent_sessions SET last_seen=17", [])
        .unwrap();
    drop(writer);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    std::fs::set_permissions(
        path.parent().unwrap(),
        std::fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let config = BoardConfig::for_database(&path);
    let mut reader =
        LocalBoard::open_read_with_timeout(&config, Duration::from_millis(100)).unwrap();
    for op in [
        BoardOp::Show {
            target: Some(BoardRef::Plan(plan)),
        },
        BoardOp::Review {
            base: super::super::board_ids::PlanRevision::new(plan, 1).unwrap(),
            agent: None,
        },
        BoardOp::FeedbackList { open_only: false },
    ] {
        reader
            .handle(&BoardRequest::new(actor("codex", "unseen-reader"), op))
            .unwrap();
    }
    assert_eq!(reader.conn.total_changes(), 0);
    assert!(
        reader.reader.is_none(),
        "readonly handles keep exactly one connection"
    );
    assert!(
        reader
            .conn
            .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
            .unwrap()
    );
    let error = reader
        .handle(&BoardRequest::new(
            actor("codex", "unseen-reader"),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new("forbidden write").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(reader.conn.total_changes(), 0);
    assert_eq!(
        reader
            .conn
            .query_row("SELECT count(*) FROM actors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        reader
            .conn
            .query_row("SELECT last_seen FROM agent_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert!(LocalBoard::open(&config).is_err());
    drop(reader);
    std::fs::set_permissions(
        path.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
