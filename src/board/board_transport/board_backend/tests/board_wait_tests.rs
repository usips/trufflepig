use super::*;

#[test]
fn unchanged_waiting_inbox_uses_sequence_gate_and_returns_empty_under_writer_lock() {
    let directory = crate::board::board_test_support::scratch("board-transport-");
    let config = BoardConfig::for_database(directory.path().join("board.sqlite3"));
    drop(LocalBoard::open(&config).unwrap());
    let host = BoardHost::with_config(config.clone());
    let mut external = rusqlite::Connection::open(&config.db_path).unwrap();
    let _writer = external
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let request = BoardRequest::new(
        config.actor(None, Some("waiting-reader")).unwrap(),
        BoardOp::Inbox {
            after: Some(EventSeq::new(0)),
            limit: 20,
            repo_key: None,
            all: true,
        },
    );
    let reply = host
        .wait_inbox(&request, QueryDeadline::after(Duration::from_millis(2200)))
        .unwrap();
    let BoardResult::Inbox(inbox) = reply.result else {
        panic!("expected inbox");
    };
    assert!(inbox.events.is_empty());
    assert_eq!(inbox.wait, InboxWait::Timeout);
    assert_eq!(host.inner.inbox_queries.load(Ordering::Relaxed), 1);
    assert!(host.inner.backend.lock().unwrap().is_none());
    assert!(waiter_transient(&anyhow::anyhow!(
        "timed_out: no deadline remains"
    )));
    assert!(waiter_transient(&anyhow::Error::new(BoardError::new(
        crate::board::board_protocol::BoardErrorCode::DatabaseLocked,
        "writer held"
    ))));
    assert!(!waiter_transient(&anyhow::anyhow!(
        "invalid_body: text mentions timed_out"
    )));
    let request = BoardRequest::new(
        config.actor(None, Some("waiting-advancer")).unwrap(),
        BoardOp::Inbox {
            after: None,
            limit: 20,
            repo_key: None,
            all: true,
        },
    );
    let reply = host
        .wait_inbox(&request, QueryDeadline::after(Duration::from_millis(50)))
        .unwrap();
    let BoardResult::Inbox(inbox) = reply.result else {
        panic!("expected empty inbox");
    };
    assert_eq!(inbox.wait, InboxWait::Timeout);
    assert!(inbox.events.is_empty());
    assert!(!inbox.advancing);
    assert_eq!(inbox.cursor, EventSeq::new(0));
}

#[test]
fn board_waiter_capacity_releases_permits_on_drop() {
    let waiters = AtomicUsize::new(0);
    let mut admitted: Vec<_> = (0..MAX_WAITERS)
        .map(|_| BoardInboxWaiterPermit::acquire(&waiters).unwrap())
        .collect();
    assert!(BoardInboxWaiterPermit::acquire(&waiters).is_none());
    admitted.pop();
    assert!(BoardInboxWaiterPermit::acquire(&waiters).is_some());
    drop(admitted);
    assert_eq!(waiters.load(Ordering::Acquire), 0);
}
