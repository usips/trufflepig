use super::*;
use std::{
    fs,
    os::unix::net::UnixStream,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn read_command() -> BoardCommand {
    BoardCommand::Op(BoardOp::Projects)
}

#[test]
fn local_board_read_deadline_caps_longer_input_at_query_deadline() {
    let incoming = QueryDeadline::after(Duration::from_secs(28));
    let incoming_remaining = incoming.remaining();
    let deadline = local_read_deadline(&read_command(), incoming);
    let remaining = deadline.remaining();

    assert!(remaining <= QUERY_DEADLINE);
    assert!(incoming_remaining.saturating_sub(remaining) >= Duration::from_secs(7));
}

#[test]
fn local_board_read_deadline_preserves_earlier_expiry() {
    let incoming = QueryDeadline::after(Duration::ZERO);
    let deadline = local_read_deadline(&read_command(), incoming);

    assert!(deadline.time_expired());
}

#[test]
fn local_board_read_deadline_preserves_peer_cancellation() {
    let (peer, disconnect) = UnixStream::pair().unwrap();
    let incoming = QueryDeadline::after(Duration::from_secs(28)).with_peer(Arc::new(peer));
    let deadline = local_read_deadline(&read_command(), incoming);

    assert!(!deadline.cancelled());
    drop(disconnect);
    assert!(deadline.cancelled());
}

#[test]
fn local_board_read_deadline_preserves_claim_cancellation() {
    let claim_path = std::env::temp_dir().join(format!(
        "trufflepig-local-board-budget-{}-{}.claim",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::write(&claim_path, b"claim").unwrap();
    let incoming =
        QueryDeadline::after(Duration::from_secs(28)).with_claim_path(claim_path.clone());
    let deadline = local_read_deadline(&read_command(), incoming);

    assert!(!deadline.cancelled());
    fs::remove_file(claim_path).unwrap();
    assert!(deadline.cancelled());
}

#[test]
fn local_board_write_and_ingest_keep_the_incoming_deadline() {
    let commands = [
        BoardCommand::Op(BoardOp::AcknowledgeInbox {
            rendered_through: crate::board::board_ids::EventSeq::new(1),
        }),
        BoardCommand::Ingest,
    ];

    for command in commands {
        let incoming = QueryDeadline::after(Duration::from_secs(28));
        let incoming_remaining = incoming.remaining();
        let deadline = local_read_deadline(&command, incoming);
        let remaining = deadline.remaining();

        assert!(remaining > QUERY_DEADLINE);
        assert!(incoming_remaining.saturating_sub(remaining) < Duration::from_secs(1));

        let expired = local_read_deadline(&command, QueryDeadline::after(Duration::ZERO));
        assert!(expired.time_expired());
    }
}
