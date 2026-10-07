use super::spool_fallback_fixture::{DIRECT_REPLY, StubDaemon, args, fixture_dir, in_child};
use crate::{cli::run_with_context, daemon::spool, diagnostics::RequestContext};
use anyhow::Result;

#[test]
fn spool_timeout_falls_back_to_direct() -> Result<()> {
    if !in_child("spool_timeout_falls_back_to_direct")? {
        return Ok(());
    }
    let root = StubDaemon::root()?;
    let context = RequestContext::new(None, None);
    spool::expire_after_next_publication();
    let result = run_with_context(&args(), &context);
    let request = root.finish()?;
    assert!(
        spool::publication_expired(),
        "the real spool request must be published"
    );
    assert_pending_removed(&context)?;
    let reply = result?;
    assert_eq!(reply, DIRECT_REPLY);
    let request = request.expect("direct execution must contact the root daemon");
    assert_eq!(
        request["arguments"]["context"]["request_id"],
        context.request_id
    );
    assert_eq!(
        request["arguments"]["args"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        "status"
    );
    println!("direct reply observed; published request and temporary file removed");
    Ok(())
}

#[test]
fn socket_timeout_does_not_fall_back() -> Result<()> {
    if !in_child("socket_timeout_does_not_fall_back")? {
        return Ok(());
    }
    assert_remote_timeout("timed_out: query deadline expired")
}

#[test]
fn remote_spool_timeout_text_does_not_fall_back() -> Result<()> {
    if !in_child("remote_spool_timeout_text_does_not_fall_back")? {
        return Ok(());
    }
    assert_remote_timeout("timed_out: spooled request deadline expired")
}

fn assert_remote_timeout(message: &str) -> Result<()> {
    let root = StubDaemon::root()?;
    let router = StubDaemon::router_timeout(message)?;
    let context = RequestContext::new(None, None);
    let result = run_with_context(&args(), &context);
    let routed_request = router.finish()?;
    let direct_request = root.finish()?;
    assert_eq!(
        result.unwrap_err().to_string(),
        format!("daemon: {message}")
    );
    assert!(
        routed_request.is_some(),
        "the system router must answer the command"
    );
    assert!(
        direct_request.is_none(),
        "answered timeouts must not reach the root daemon"
    );
    assert!(!spool::publication_expired());
    assert_pending_removed(&context)?;
    Ok(())
}

fn assert_pending_removed(context: &RequestContext) -> Result<()> {
    let directory = fixture_dir().join("s");
    for suffix in ["request", "request.tmp", "reply", "claimed"] {
        assert!(
            !directory
                .join(format!("{}.{suffix}", context.request_id))
                .exists()
        );
    }
    let entries: Vec<_> = std::fs::read_dir(directory)?.collect::<std::io::Result<_>>()?;
    assert_eq!(entries.len(), 1, "only the unrelated heartbeat must remain");
    assert_eq!(entries[0].file_name(), "heartbeat");
    Ok(())
}
