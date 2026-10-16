use super::{CLIENT_REPLY_WAIT, DaemonReply, DaemonReplyTimeout, encoded_reply};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum LegacyDaemonReply {
    Success { output: String },
    Failure { message: String },
}

fn decode_legacy_socket_frame(frame: &[u8]) -> LegacyDaemonReply {
    let (header, payload) = frame.split_at(4);
    let length = u32::from_be_bytes(header.try_into().unwrap()) as usize;
    assert_eq!(payload.len(), length);
    serde_json::from_slice(payload).unwrap()
}

fn assert_failure(reply: LegacyDaemonReply, expected_message: &str) {
    let LegacyDaemonReply::Failure { message } = reply else {
        panic!("legacy decoder should see the timeout as an ordinary failure");
    };
    assert_eq!(message, expected_message);
}

#[test]
fn new_timeout_replies_decode_as_failures_for_legacy_socket_and_spool_clients() {
    let timeout = DaemonReplyTimeout::new(std::time::Duration::from_millis(37), CLIENT_REPLY_WAIT);
    let message = timeout.to_string();
    let reply = DaemonReply::from_result(Err(anyhow::Error::new(timeout).context("forwarded")));
    let DaemonReply::Failure {
        message: wire_message,
    } = &reply
    else {
        panic!("new timeout replies must use the legacy Failure status");
    };
    assert_eq!(wire_message, &message);

    let socket_frame = encoded_reply(&reply);
    assert_failure(decode_legacy_socket_frame(&socket_frame), &message);

    let spool_json = serde_json::to_vec(&reply).unwrap();
    let spool_reply: LegacyDaemonReply = serde_json::from_slice(&spool_json).unwrap();
    assert_failure(spool_reply, &message);
}

#[test]
fn legacy_decoder_rejects_the_removed_reply_timeout_status() {
    let old_wire = serde_json::json!({
        "status": "reply_timeout",
        "failure": {
            "wait_ms": 37,
            "deadline_ms": 30000,
            "retry_after_ms": 250
        }
    });

    assert!(serde_json::from_value::<LegacyDaemonReply>(old_wire).is_err());
}
