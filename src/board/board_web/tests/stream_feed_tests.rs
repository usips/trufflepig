use super::*;

#[test]
fn event_streams_share_a_dedicated_connection_outside_the_reader_pool() {
    let directory = crate::board::board_test_support::scratch("web-stream-feed-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = Arc::new(
        WebStore::open_at(
            BoardConfigCache::with_config(config.clone()),
            directory.path().join("runtime"),
        )
        .unwrap(),
    );
    for title in ["First plan", "Second plan"] {
        read(
            &store,
            BoardOp::New {
                title: PlanTitle::new(title).unwrap(),
                body: PlanText::new("seed\n").unwrap(),
                steward: None,
                repo_key: None,
            },
        );
    }
    let (poller, streams) = open_stream_feed(&store).unwrap();
    assert_eq!(store.readers.checkout_count(), 0, "writes use the writer");
    let mut first = subscribe(&streams);
    let mut second = subscribe(&streams);
    let replay_a = read_frame_until(&mut first, "id: 2\n");
    let replay_b = read_frame_until(&mut second, "id: 2\n");
    assert!(replay_a.contains("id: 1\n"), "{replay_a}");
    assert_eq!(
        replay_a.split("\r\n\r\n").nth(1),
        replay_b.split("\r\n\r\n").nth(1),
        "both subscribers replay the same ring history"
    );
    assert_eq!(
        store.readers.checkout_count(),
        0,
        "stream replay never checks out a pooled reader"
    );
    for _ in 0..3 {
        let reply = read(&store, overview());
        assert!(matches!(reply.result, BoardResult::Overview(_)));
    }
    assert_eq!(
        store.readers.checkout_count(),
        3,
        "only the GETs drew from the four-connection pool"
    );
    assert_eq!(
        store.readers.pool_size(),
        4,
        "the dedicated feed connection leaves the request pool whole"
    );
    read(
        &store,
        BoardOp::New {
            title: PlanTitle::new("Live plan").unwrap(),
            body: PlanText::new("live\n").unwrap(),
            steward: None,
            repo_key: None,
        },
    );
    // The real 250 ms poller drives the write into both shared streams.
    assert!(read_frame_until(&mut first, "id: 3\n").contains("id: 3\n"));
    assert!(read_frame_until(&mut second, "id: 3\n").contains("id: 3\n"));
    assert_eq!(
        store.readers.checkout_count(),
        3,
        "the live fill used the dedicated feed connection"
    );
    drop(first);
    drop(second);
    let started = Instant::now();
    while streams.active() != 0 {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stream permit retained"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(streams);
    drop(poller);
}
