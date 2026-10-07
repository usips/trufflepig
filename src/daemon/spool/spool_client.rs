//! Deadline-bound spool requests and reply polling.

use super::*;

/// Sends a request until `deadline`; `None` means no daemon drains it.
pub fn request(
    spool: &Path,
    args: &[String],
    context: &crate::diagnostics::RequestContext,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    check_deadline(&deadline)?;
    let request = DaemonRequest::Arguments {
        context: context.clone(),
        args: args.to_vec(),
    };
    let validation = request.validate();
    check_deadline(&deadline)?;
    validation?;
    let serialized = serde_json::to_vec(&request);
    check_deadline(&deadline)?;
    let bytes = serialized?;

    let parsed_id = uuid::Uuid::parse_str(&context.request_id);
    check_deadline(&deadline)?;
    let id = parsed_id?.to_string();
    let request_path = spool.join(format!("{id}.request"));
    let reply_path = spool.join(format!("{id}.reply"));
    let result = request_until(spool, &request_path, &reply_path, &bytes, deadline);
    if result.as_ref().err().is_some_and(deadline::is_timed_out) {
        remove_pending_request(&request_path);
    }
    result
}

fn request_until(
    spool: &Path,
    request_path: &Path,
    reply_path: &Path,
    bytes: &[u8],
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    if !heartbeat_alive(spool, &deadline)? {
        return Ok(None);
    }
    check_deadline(&deadline)?;
    let write_result = write_request_atomic(request_path, bytes, &deadline, |temporary, bytes| {
        fs::write(temporary, bytes)
    });
    check_deadline(&deadline)?;
    if let Err(error) = write_result {
        let spool_unavailable = error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<std::io::Error>(),
                Some(io)
                    if matches!(
                        io.kind(),
                        ErrorKind::NotFound
                            | ErrorKind::PermissionDenied
                            | ErrorKind::ReadOnlyFilesystem
                    )
            )
        });
        if spool_unavailable {
            return Ok(None);
        }
        return Err(error).context("write spooled daemon request");
    }
    loop {
        check_deadline(&deadline)?;
        let read_result = fs::read(reply_path);
        check_deadline(&deadline)?;
        match read_result {
            Ok(bytes) => {
                let decoded = serde_json::from_slice(&bytes).context("decode spooled daemon reply");
                check_deadline(&deadline)?;
                let reply: DaemonReply = decoded?;
                let _ = fs::remove_file(reply_path);
                check_deadline(&deadline)?;
                return match reply {
                    DaemonReply::Success { output } => Ok(Some(output)),
                    DaemonReply::Failure { message } => bail!("daemon: {message}"),
                };
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("read spooled daemon reply"),
        }
        if !heartbeat_alive(spool, &deadline)? {
            check_deadline(&deadline)?;
            if fs::remove_file(request_path).is_ok() {
                check_deadline(&deadline)?;
                return Ok(None);
            }
            // Claimed by a router that stopped beating: no reply is coming.
            if let Some(id) = request_id(request_path, "request") {
                let _ = fs::remove_file(spool.join(format!("{id}.claimed")));
            }
            check_deadline(&deadline)?;
            bail!("daemon_unavailable: router stopped while answering a spooled request");
        }
        let wait = deadline.cap(REPLY_POLL);
        if wait.is_zero() {
            check_deadline(&deadline)?;
        }
        std::thread::sleep(wait);
    }
}

pub(super) fn check_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        bail!("{TIMED_OUT}: spooled request deadline expired");
    }
    Ok(())
}

fn remove_pending_request(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(temporary_path(path));
}
