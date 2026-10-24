use super::super::admission_refusal::{AdmissionRefusal, RefusalCause};
use super::super::{DaemonReply, protocol, refuse_busy_nonblocking};
use crate::diagnostics::RequestContext;
use std::io::{Error, ErrorKind};
use std::os::unix::net::UnixStream;

#[test]
fn refused_socket_reply_keeps_busy_code_and_bounded_queue_diagnostics() {
    let (mut client, server) = UnixStream::pair().unwrap();
    let refusal = AdmissionRefusal::new("daemon-ingress", 8, 8, RefusalCause::QueueFull)
        .with_active_verb(Some("search"));
    let refusal_message = refusal.message();
    refuse_busy_nonblocking(&server, refusal);

    protocol::write_request(
        &mut client,
        &protocol::DaemonRequest::Arguments {
            context: RequestContext::new(None, None),
            args: vec!["search".to_owned(), "private query".to_owned()],
        },
    )
    .unwrap();
    let reply = protocol::read_reply(&mut client).unwrap();
    let DaemonReply::Failure { message } = reply else {
        panic!("refusal must use the failure reply variant");
    };
    assert_eq!(message, refusal_message);
    assert_eq!(
        crate::coded_error::ErrorCode::leading(&message).map(|(code, _)| code),
        Some(crate::coded_error::ErrorCode::DaemonBusy)
    );
    assert!(message.contains("pool=daemon-ingress queue_depth=8 queue_capacity=8"));
    assert!(message.contains("cause=queue_full retry_after_ms=250"));
    assert!(message.contains("hint=retry_with_bounded_backoff active_verb=search"));
    assert!(!message.contains("private query"));
    assert!(!message.contains("index_warming"));
    assert!(!message.contains("daemon_reply_timeout"));
}

#[test]
fn clone_failure_classifies_only_descriptor_exhaustion() {
    for code in [libc::EMFILE, libc::ENFILE] {
        let error = Error::from_raw_os_error(code);
        let message = AdmissionRefusal::clone_failed("ingress", 2, 4, &error).message();
        assert!(message.contains("cause=fd_exhausted"), "{message}");
    }

    let error = Error::from_raw_os_error(libc::EINVAL);
    let message = AdmissionRefusal::clone_failed("ingress", 2, 4, &error).message();
    assert!(message.contains("cause=stream_clone_failed"), "{message}");

    let error = Error::new(ErrorKind::Other, "private query and source path");
    let message = AdmissionRefusal::clone_failed("ingress", 2, 4, &error).message();
    assert!(message.contains("cause=stream_clone_failed"), "{message}");
    assert!(!message.contains("private query"));
    assert!(!message.contains("source path"));
}

#[test]
fn unsafe_labels_are_omitted_without_echoing_input() {
    let message = AdmissionRefusal::new(
        "ingress /home/user/private.rs",
        usize::MAX,
        4,
        RefusalCause::PoolClosed,
    )
    .with_active_verb(Some("search private query"))
    .message();

    assert!(message.contains("pool=unknown"));
    assert!(!message.contains("/home/user/private.rs"));
    assert!(!message.contains("private query"));
    assert!(!message.contains("active_verb="));
    assert!(message.len() < DIAGNOSTIC_LIMIT);
}

#[test]
fn maximum_valid_labels_and_queue_counts_stay_within_diagnostic_bound() {
    let pool = "p".repeat(48);
    let active_verb = "v".repeat(48);
    let message = AdmissionRefusal::new(
        &pool,
        usize::MAX,
        usize::MAX,
        RefusalCause::StreamCloneFailed,
    )
    .with_active_verb(Some(&active_verb))
    .with_root_limit(usize::MAX, usize::MAX)
    .message();

    assert!(message.contains(&format!("pool={pool}")));
    assert!(message.contains(&format!("active_verb={active_verb}")));
    assert!(message.contains(&format!(
        "root_outstanding={} root_capacity={}",
        usize::MAX,
        usize::MAX
    )));
    assert!(message.len() <= DIAGNOSTIC_LIMIT, "{} bytes", message.len());

    let oversized_pool = "p".repeat(49);
    let oversized_verb = "v".repeat(49);
    let message = AdmissionRefusal::new(
        &oversized_pool,
        usize::MAX,
        usize::MAX,
        RefusalCause::StreamCloneFailed,
    )
    .with_active_verb(Some(&oversized_verb))
    .message();
    assert!(message.contains("pool=unknown"));
    assert!(!message.contains("active_verb="));
}

const DIAGNOSTIC_LIMIT: usize = 400;
