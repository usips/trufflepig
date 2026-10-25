mod reply_timeout_tests;
mod spool_transport_tests;

use super::daemon_wire_protocol::DaemonReply;
use super::*;
use std::fs::{self, File};
use std::io::Cursor;
use std::os::unix::net::UnixListener;
use std::time::SystemTime;

/// Disk-backed scratch for socket and spool fixtures.
pub(super) fn scratch() -> tempfile::TempDir {
    crate::test_fixtures::scratch("trufflepig-transport-")
}

#[test]
fn daemon_protocol_rejects_oversized_and_truncated_frames() {
    let header = ((daemon_wire_protocol::REQUEST_LIMIT + 1) as u32).to_be_bytes();
    assert!(daemon_wire_protocol::read_request(&mut Cursor::new(header)).is_err());
    let bytes = [0, 0, 0, 8, b'{'];
    assert!(daemon_wire_protocol::read_request(&mut Cursor::new(bytes)).is_err());
    let request = DaemonRequest::Arguments {
        context: crate::request_context::RequestContext::new(None, None),
        args: vec![String::new(); 257],
    };
    assert!(daemon_wire_protocol::write_request(&mut Vec::new(), &request).is_err());
}

#[test]
fn daemon_protocol_bounds_complete_serialized_response() {
    let reply = DaemonReply::Success {
        output: "\0".repeat(1024 * 1024),
    };
    let mut wire = Vec::new();
    daemon_wire_protocol::write_reply(&mut wire, &reply).unwrap();
    assert!(wire.len() < 1024);
    assert!(matches!(
        daemon_wire_protocol::read_reply(&mut Cursor::new(wire)).unwrap(),
        DaemonReply::Failure { .. }
    ));
}

#[test]
fn daemon_connect_unreachability_includes_sandbox_permission_denial() {
    assert!(unreachable(ErrorKind::NotFound));
    assert!(unreachable(ErrorKind::ConnectionRefused));
    assert!(unreachable(ErrorKind::PermissionDenied));
    assert!(unreachable(ErrorKind::NotADirectory));
    assert!(!unreachable(ErrorKind::ConnectionReset));
    assert!(!unreachable(ErrorKind::TimedOut));
}

#[test]
fn encoded_request_limit_counts_json_escaping() {
    let context = crate::request_context::RequestContext::new(None, None);
    let args = vec![
        "board".to_owned(),
        "\"".repeat(MAX_DAEMON_REQUEST_BYTES / 2),
    ];
    let bytes = request_encoded_size(&args, &context).unwrap();
    assert!(args.iter().map(String::len).sum::<usize>() < MAX_DAEMON_REQUEST_BYTES);
    assert!(bytes > MAX_DAEMON_REQUEST_BYTES);
    let request = arguments(&args, &context);
    assert_eq!(bytes, serde_json::to_vec(&request).unwrap().len());
}
