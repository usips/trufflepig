//! Client side of the daemon wire: length-framed JSON over Unix sockets, a
//! file spool for sandboxed clients, and the limits and waits that clients,
//! forwarding daemons, and servers share. Servers live in `crate::daemon`.

pub(crate) mod daemon_wire_protocol;
pub(crate) mod socket_connect;
pub mod spool;
#[cfg(test)]
mod tests;
mod transport_failure;

use crate::coded_error::CodedContext;
use crate::query_deadline::QueryDeadline;
use crate::request_context::RequestContext;
use crate::runtime_paths::{spool_dir, system_dir};
use anyhow::{Context, Result, ensure};
use daemon_wire_protocol::DaemonRequest;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) use transport_failure::{DaemonReplyTimeout, DeadlineRead, DeadlineWrite};

pub(crate) const SOCKET_NAME: &str = "daemon.sock";
/// Longest a client waits for any daemon reply, over the socket or the spool.
pub const CLIENT_REPLY_WAIT: Duration = Duration::from_secs(30);
/// Longest a daemon waits on the daemon it forwards to; shorter than
/// [`CLIENT_REPLY_WAIT`] so the client receives the forwarding failure.
pub const PROXY_REPLY_WAIT: Duration = Duration::from_secs(28);
const REQUEST_WRITE_WAIT: Duration = Duration::from_secs(5);
pub(crate) const RUNNING_PROBE_WAIT: Duration = Duration::from_millis(100);

/// Maximum encoded JSON request size, excluding its length prefix.
pub(crate) const MAX_DAEMON_REQUEST_BYTES: usize = daemon_wire_protocol::REQUEST_LIMIT;

/// Measures the exact wire request before client-side routing or fallback.
pub(crate) fn request_encoded_size(args: &[String], context: &RequestContext) -> Result<usize> {
    Ok(serde_json::to_vec(&arguments(args, context))?.len())
}

/// Returns `None` only when no daemon is listening; protocol errors stay errors.
pub fn request(cache: &Path, args: &[String], context: &RequestContext) -> Result<Option<String>> {
    request_by(
        cache,
        args,
        context,
        QueryDeadline::after(CLIENT_REPLY_WAIT),
    )
}

/// [`request`] waiting for the reply only until `deadline`, so a daemon that
/// forwards never waits longer than the client in front of it.
pub fn request_by(
    cache: &Path,
    args: &[String],
    context: &RequestContext,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    exchange(cache, &arguments(args, context), deadline)
}

fn arguments(args: &[String], context: &RequestContext) -> DaemonRequest {
    DaemonRequest::Arguments {
        context: context.clone(),
        args: args.to_vec(),
    }
}

/// Stops a listening daemon through its authenticated-by-filesystem socket.
pub fn stop(cache: &Path) -> Result<String> {
    Ok(exchange(
        cache,
        &DaemonRequest::Stop,
        QueryDeadline::after(CLIENT_REPLY_WAIT),
    )?
    .unwrap_or_else(|| "{\"status\":\"not_running\"}".to_owned()))
}

/// Whether a daemon listens on `cache`'s socket. Its 100 ms probe never
/// contends for the startup lock; a timed-out probe conservatively reports
/// that a busy daemon may still be running.
pub fn running(cache: &Path) -> bool {
    match running_by(cache, &QueryDeadline::after(RUNNING_PROBE_WAIT)) {
        Ok(is_running) => is_running,
        Err(error) if connect_timed_out(&error) => true,
        Err(_) => false,
    }
}

fn connect_timed_out(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == ErrorKind::TimedOut)
    })
}

/// Checks for a daemon without treating an expired or cancelled connect as absence.
pub(crate) fn running_by(cache: &Path, deadline: &QueryDeadline) -> Result<bool> {
    let path = cache.join(SOCKET_NAME);
    match socket_connect::connect_by(&path, deadline) {
        Ok(_stream) => Ok(true),
        Err(error) if unreachable(error.kind()) => Ok(false),
        Err(error) if error.kind() == ErrorKind::TimedOut => {
            Err(error).with_coded_context(|| coded!(TimedOut, "phase=connecting"))
        }
        Err(error) => Err(error).context("connect to repository daemon"),
    }
}

/// Sends a request to the system daemon over its socket, else its spool;
/// `None` means no router answered either way.
pub fn router_request(args: &[String], context: &RequestContext) -> Result<Option<String>> {
    router_request_by(args, context, QueryDeadline::after(CLIENT_REPLY_WAIT))
}

/// Sends a system request through the router, then spool, under one deadline.
pub(crate) fn router_request_by(
    args: &[String],
    context: &RequestContext,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    if let Some(dir) = system_dir()
        && let Some(reply) = request_by(&dir, args, context, deadline.clone())?
    {
        return Ok(Some(reply));
    }
    spool::request(&spool_dir(), args, context, deadline)
}

/// Sandboxed clients get EPERM/EACCES from `connect`; treat as "no daemon".
fn unreachable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::NotFound
            | ErrorKind::NotADirectory
            | ErrorKind::ConnectionRefused
            | ErrorKind::PermissionDenied
    )
}

fn exchange(
    cache: &Path,
    request: &DaemonRequest,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    request.validate()?;
    let wait_budget = deadline.remaining();
    ensure!(
        !wait_budget.is_zero(),
        coded!(TimedOut, "phase=connecting no time left to reach a daemon")
    );
    let socket_path = cache.join(SOCKET_NAME);
    let mut stream = match socket_connect::connect_by(&socket_path, &deadline) {
        Ok(stream) => stream,
        Err(error) if unreachable(error.kind()) => {
            return Ok(None);
        }
        Err(error) if error.kind() == ErrorKind::TimedOut => {
            return Err(error).with_coded_context(|| coded!(TimedOut, "phase=connecting"));
        }
        Err(error) => return Err(error).context("connect to repository daemon"),
    };
    ensure!(
        !deadline.expired(),
        coded!(
            TimedOut,
            "phase=connecting deadline expired before request write"
        )
    );
    let write_deadline = deadline.capped(REQUEST_WRITE_WAIT);
    {
        let mut writer = DeadlineWrite::new(&mut stream, &write_deadline);
        if let Err(error) = daemon_wire_protocol::write_request(&mut writer, request) {
            let timed_out = error.chain().any(|cause| {
                cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                })
            });
            if timed_out {
                return Err(error).with_coded_context(|| coded!(TimedOut, "phase=writing_request"));
            }
            return Err(error);
        }
    }
    let reply_started = Instant::now();
    let mut reader = DeadlineRead::new(&mut stream, &deadline);
    let reply = match daemon_wire_protocol::read_reply(&mut reader) {
        Ok(reply) => reply,
        Err(error) => {
            let timed_out = error.chain().any(|cause| {
                cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                })
            });
            if timed_out {
                return Err(DaemonReplyTimeout::new(reply_started.elapsed(), wait_budget).into());
            }
            return Err(error);
        }
    };
    reply.into_result().map(Some)
}
