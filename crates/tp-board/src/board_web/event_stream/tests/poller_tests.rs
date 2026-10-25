use super::*;

#[test]
fn sequence_wakes_coalesce_and_poller_retries_at_bounded_cadence() {
    let wake = SequenceWake::new();
    let observed = wake.generation();
    for seq in 1..=100 {
        wake.publish(EventSeq::new(seq));
    }
    assert_eq!(
        wake.wait(observed, Duration::from_secs(1)),
        WakeResult::Changed
    );
    assert_eq!(
        wake.wait(wake.generation(), Duration::from_millis(1)),
        WakeResult::Timeout
    );
    let calls = Arc::new(AtomicU64::new(0));
    let called = Arc::clone(&calls);
    let poller = SequencePoller::start(Arc::new(move || {
        called.fetch_add(1, Ordering::AcqRel);
        Err(BoardError::new(BoardErrorCode::DatabaseLocked, "busy"))
    }))
    .unwrap();
    // Two ticks prove the cadence without assuming how fast they land.
    let started = Instant::now();
    while calls.load(Ordering::Acquire) < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "poller never ticked twice"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let handle = poller.handle();
    drop(poller);
    assert!((2..=4).contains(&calls.load(Ordering::Acquire)));
    assert_eq!(
        handle.wait(handle.generation(), Duration::from_secs(1)),
        WakeResult::Stopped
    );
}

#[test]
fn event_snapshot_rejects_unordered_or_cursor_overlapping_records() {
    let batch = ReplayBatch {
        latest: EventSeq::new(9),
        events: vec![event(8, "a"), event(7, "b")],
    };
    assert!(batch.validate(EventSeq::new(0)).is_err());
    let batch = ReplayBatch {
        latest: EventSeq::new(9),
        events: vec![event(8, "a")],
    };
    assert!(batch.validate(EventSeq::new(8)).is_err());
    let batch = ReplayBatch {
        latest: EventSeq::new(9),
        events: vec![event(10, "b")],
    };
    assert!(batch.validate(EventSeq::new(0)).is_err());
}

#[test]
fn polling_busy_or_panic_rejects_admission_then_recovers_unchanged_sequence() {
    for panic_first in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let called = Arc::clone(&calls);
        let allow_success = Arc::new(AtomicBool::new(false));
        let success_gate = Arc::clone(&allow_success);
        let poller = SequencePoller::start(Arc::new(move || {
            let call = called.fetch_add(1, Ordering::AcqRel);
            if !success_gate.load(Ordering::Acquire) {
                if panic_first && call == 0 {
                    panic!("sequence reader panic");
                }
                return Err(BoardError::new(BoardErrorCode::DatabaseLocked, "busy"));
            }
            Ok(EventSeq::new(0))
        }))
        .unwrap();
        let wake = poller.handle();
        assert_eq!(
            wake.wait(wake.generation(), Duration::from_secs(1)),
            WakeResult::Unavailable
        );
        let unavailable_generation = wake.generation();
        let streams = EventStreams::new(empty_reader(), wake.clone());
        assert_eq!(
            streams.reserve().err().unwrap().code,
            BoardErrorCode::BoardUnavailable
        );
        allow_success.store(true, Ordering::Release);
        let started = Instant::now();
        while !wake.available() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "poller failed to recover"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            wake.wait(unavailable_generation, Duration::ZERO),
            WakeResult::Changed
        );
        assert!(calls.load(Ordering::Acquire) >= 2);
        drop(streams.reserve().unwrap());
        assert_eq!(streams.active(), 0);
        drop(poller);
        assert_eq!(
            wake.wait(wake.generation(), Duration::ZERO),
            WakeResult::Stopped
        );
        assert_eq!(
            streams.reserve().err().unwrap().code,
            BoardErrorCode::BoardUnavailable
        );
    }
}
