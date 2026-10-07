use super::*;
use std::sync::{TryLockError, mpsc};

#[test]
fn begin_during_settle_leads_a_new_scan() {
    let flight = IngestFlight::default();
    let leader = flight.begin();
    let (settling, settled) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (attempted, attempts) = mpsc::channel();
    let (claimed, claims) = mpsc::channel();
    let rendezvous = Duration::from_secs(5);
    let shared = &flight;
    let (lock_held, blocked, next, scans) = std::thread::scope(|scope| {
        let relay = scope.spawn(move || {
            let mut scans = 0;
            let mut first_settle = true;
            relay_flight_with_settle_hook(
                shared,
                || scans += 1,
                || {
                    if first_settle {
                        first_settle = false;
                        settling.send(()).unwrap();
                        released.recv_timeout(rendezvous).unwrap();
                    }
                },
            );
            scans
        });
        settled.recv_timeout(rendezvous).unwrap();
        let beginner = scope.spawn(move || {
            // This probe establishes mutex ownership without depending on
            // whether the thread reaches begin before a timing deadline.
            let held = matches!(shared.state.try_lock(), Err(TryLockError::WouldBlock));
            attempted.send(held).unwrap();
            claimed.send(shared.begin()).unwrap();
        });
        let lock_held = attempts.recv_timeout(rendezvous).unwrap();
        let early = claims.recv_timeout(Duration::from_millis(25));
        let blocked = matches!(early, Err(mpsc::RecvTimeoutError::Timeout));
        release.send(()).unwrap();
        let next = early.unwrap_or_else(|_| claims.recv_timeout(rendezvous).unwrap());
        beginner.join().unwrap();
        (lock_held, blocked, next, relay.join().unwrap())
    });
    assert!(
        lock_held,
        "settle must hold the mutex after reading dirty=false"
    );
    assert!(
        blocked,
        "begin must block until the clean settle releases the mutex"
    );
    assert!(
        next.leads,
        "begin after the clean settle must lead a new flight"
    );
    assert_ne!(next.ticket, leader.ticket);
    assert_eq!(scans, 1, "the settled flight does not inherit the new scan");
    let mut next_scans = 0;
    relay_flight(&flight, || {
        next_scans += 1;
        assert_eq!(flight.current_ticket(), next.ticket);
    });
    assert_eq!(next_scans, 1);
    assert!(
        flight.begin().leads,
        "the new flight also releases its slot"
    );
}
