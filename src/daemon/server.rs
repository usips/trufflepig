//! A nonblocking accept loop assembles bounded frames before tier-specific pools run them.
//! Per-root permits limit expensive work while a maintenance thread runs beside requests.
//! Panics become failures or stop the daemon without stranding accepted sockets.
use super::DaemonSocket;
use super::pool::{PoolSize, PoolSubmitCause, PooledJob, RequestClass, RequestLane};
use super::protocol::{self, DaemonReply, DaemonRequest};
use super::reconciler::{self, Maintenance};
use super::spool::{ClaimedSpoolRequest, SpoolServer};
use crate::coded_error::CodedContext;
use crate::query_deadline::QueryDeadline;
use crate::request_context::RequestContext;
use admission_refusal::{AdmissionRefusal, RefusalCause};
use anyhow::{Context, Result};
use ingress_frames::{
    CompleteFrame, FinishedFrame, FrameFailure, FramePoll, PendingFrame, hold_refused,
    poll_refused, wait_for_ingress,
};
use semantic_fallback::SemanticAdmissionFallback;
use std::any::Any;
use std::io::ErrorKind;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

mod admission_refusal;
mod ingress_frames;
mod request_admission;
mod root_work_limits;
mod semantic_fallback;

use request_admission::AdmissionDecision;
pub(super) use request_admission::RequestAdmission;

/// Longest idle wait of the accept loop before it polls its peers again.
const ACCEPT_POLL: Duration = Duration::from_millis(10);
/// Time a request frame may take to arrive, and a reply to drain.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Leaves bounded headroom for a timeout failure after work has expired.
const EXPIRED_REPLY_GRACE: Duration = Duration::from_millis(250);
const ACCEPTS_PER_TURN: usize = 16;
/// How long a stopping server waits for a reconcile in progress before returning.
const MAINTENANCE_STOP_WAIT: Duration = Duration::from_secs(1);
/// How long a stopping server lets accepted requests finish; their clients wait
/// no longer than this for a forwarded reply anyway.
const STOP_DRAIN: Duration = super::PROXY_REPLY_WAIT;
/// One accepted request; `deadline` starts when the connection is accepted.
pub struct AcceptedRequest {
    pub context: RequestContext,
    pub args: Vec<String>,
    pub deadline: QueryDeadline,
}

/// Daemon behavior shared by request workers and the maintenance thread.
pub trait DaemonHandler: Send + Sync + 'static {
    fn request(&self, request: AcceptedRequest) -> Result<String>;
    /// Blocking startup work after the socket binds and before the accept
    /// loop: waiting clients queue in the listen backlog while it runs, and
    /// no request races it.
    fn on_bound(&self) {}
    /// Reconciles the served root; runs at startup and when changes are due.
    fn reconcile(&self) -> Result<()> {
        Ok(())
    }
    /// Periodic maintenance on the maintenance thread.
    fn idle(&self) {}
}

/// Stop flag plus the first fatal maintenance error.
#[derive(Default)]
pub(super) struct ServerState {
    stopping: AtomicBool,
    failure: Mutex<Option<anyhow::Error>>,
}

impl ServerState {
    pub(super) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }
    pub(super) fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }
    pub(super) fn fail(&self, error: anyhow::Error) {
        if let Ok(mut failure) = self.failure.lock() {
            failure.get_or_insert(error);
        }
        self.stop();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdmissionTier {
    Router,
    Coordinator,
    Root,
}

impl AdmissionTier {
    pub(super) const fn accepted_budget(self) -> Duration {
        match self {
            Self::Router => Duration::from_secs(28),
            Self::Coordinator => Duration::from_secs(24),
            Self::Root => crate::query_deadline::QUERY_DEADLINE,
        }
    }

    const fn pending_frame_capacity(self) -> usize {
        match self {
            Self::Root | Self::Coordinator => 64,
            Self::Router => 128,
        }
    }
}

/// How one daemon tier serves.
pub(super) struct ServeProfile<'a> {
    pub name: &'a str,
    pub watching: bool,
    pub pool: PoolSize,
    pub tier: AdmissionTier,
    pub spool: Option<&'a Path>,
}

/// Serves `cache`'s socket until a `stop` request, a vanished watched root, or a
/// failed initial reconcile (returned as the error).
pub(super) fn serve<H: DaemonHandler>(
    root: &Path,
    cache: &Path,
    profile: ServeProfile<'_>,
    handler: H,
) -> Result<()> {
    let root = root.canonicalize().context("resolve watched repository")?;
    std::fs::create_dir_all(cache)?;
    let cache = cache.canonicalize()?;
    let socket = DaemonSocket::bind(&cache)?;
    // Opened after the startup lock so a losing racer never discards live requests.
    let spool = profile.spool.map(SpoolServer::open).transpose()?;
    let handler = Arc::new(handler);
    // Blocking startup work (router migration) runs after the bind, before the
    // accept loop, so waiting clients queue instead of spawning a second daemon.
    handler.on_bound();
    let admission = Arc::new(RequestAdmission::new(
        profile.name,
        profile.tier,
        profile.pool,
    )?);
    let state = Arc::new(ServerState::default());
    let (finished, maintenance_done) = mpsc::channel::<()>();
    {
        let (handler, admission, state) = (
            Arc::clone(&handler),
            Arc::clone(&admission),
            Arc::clone(&state),
        );
        let maintenance = Maintenance {
            root,
            cache: cache.clone(),
            watching: profile.watching,
            spool,
        };
        std::thread::Builder::new()
            .name(format!("{}-reconciler", profile.name))
            .spawn(move || {
                reconciler::run(maintenance, &handler, &admission, &state);
                drop(finished);
            })?;
    }
    let accepted = accept_until_stopped(
        &socket,
        &handler,
        &admission,
        &state,
        admission.accepted_budget(),
    );
    // A reconcile in progress may outlive the server; its writer lease keeps the
    // index coherent until the daemon process exits.
    let _ = maintenance_done.recv_timeout(MAINTENANCE_STOP_WAIT);
    // A successor may bind now; requests already accepted still get replies.
    drop(socket);
    let drained = admission.drain(STOP_DRAIN);
    admission.shutdown();
    if !drained {
        eprintln!(
            "trufflepig: daemon request drain exceeded {STOP_DRAIN:?}; stopping with unfinished requests"
        );
    }
    accepted?;
    match state
        .failure
        .lock()
        .ok()
        .and_then(|mut failure| failure.take())
    {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn accept_until_stopped<H: DaemonHandler>(
    socket: &DaemonSocket,
    handler: &Arc<H>,
    admission: &Arc<RequestAdmission>,
    state: &Arc<ServerState>,
    accepted_budget: Duration,
) -> Result<()> {
    let capacity = admission.pending_frame_capacity();
    let mut pending: Vec<PendingFrame> = Vec::with_capacity(capacity);
    let mut refused: Vec<PendingFrame> = Vec::with_capacity(capacity);
    while !state.stopping() {
        let mut made_progress = poll_pending(&mut pending, handler, admission, state);
        made_progress |= poll_refused(&mut refused);

        for _ in 0..ACCEPTS_PER_TURN {
            match socket.listener.accept() {
                Ok((stream, _)) => {
                    made_progress = true;
                    if pending.len() == capacity {
                        let snapshot = admission.ingress().snapshot();
                        let refusal = AdmissionRefusal::new(
                            admission.ingress().name(),
                            snapshot.queue_depth,
                            snapshot.queue_capacity,
                            RefusalCause::QueueFull,
                        )
                        .with_active_verb(snapshot.active_verb);
                        refuse_busy_nonblocking(&stream, refusal);
                        hold_refused(&mut refused, stream, capacity);
                        continue;
                    }
                    let deadline = QueryDeadline::after(accepted_budget);
                    match PendingFrame::new(stream, deadline, FRAME_TIMEOUT) {
                        Ok(frame) => pending.push(frame),
                        Err(error) => eprintln!(
                            "trufflepig: could not configure pending daemon frame: {error}"
                        ),
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("accept daemon request"),
            }
        }
        if !made_progress {
            wait_for_ingress(&socket.listener, &pending, &refused, ACCEPT_POLL);
        }
    }
    // Frames accepted before the stop still get their answers. The listener takes
    // no new connections here, and each frame ends within its own budget.
    while !pending.is_empty() || !refused.is_empty() {
        let progressed =
            poll_pending(&mut pending, handler, admission, state) | poll_refused(&mut refused);
        if !progressed {
            std::thread::sleep(ACCEPT_POLL);
        }
    }
    Ok(())
}

/// Polls each pending frame once, dispatching those that finish. Reports whether
/// any peer made progress, so an idle turn is the only turn that waits.
fn poll_pending<H: DaemonHandler>(
    pending: &mut Vec<PendingFrame>,
    handler: &Arc<H>,
    admission: &Arc<RequestAdmission>,
    state: &Arc<ServerState>,
) -> bool {
    let mut progressed = false;
    let mut index = 0;
    while index < pending.len() {
        match pending[index].poll() {
            FramePoll::Idle => index += 1,
            FramePoll::Progressed => {
                progressed = true;
                index += 1;
            }
            FramePoll::Finished => {
                progressed = true;
                let finished = pending.swap_remove(index).finish();
                dispatch_finished_frame(finished, handler, admission, state);
            }
        }
    }
    progressed
}

fn dispatch_finished_frame<H: DaemonHandler>(
    finished: FinishedFrame,
    handler: &Arc<H>,
    admission: &Arc<RequestAdmission>,
    state: &Arc<ServerState>,
) {
    let (stream, label, job): (UnixStream, &'static str, PooledJob) = match finished {
        FinishedFrame::Complete(CompleteFrame {
            stream,
            bytes,
            deadline,
        }) => {
            let (handler, admission, state) = (
                Arc::clone(handler),
                Arc::clone(admission),
                Arc::clone(state),
            );
            let response_stream = match stream.try_clone() {
                Ok(stream) => stream,
                Err(error) => {
                    let snapshot = admission.ingress().snapshot();
                    let refusal = AdmissionRefusal::clone_failed(
                        admission.ingress().name(),
                        snapshot.queue_depth,
                        snapshot.queue_capacity,
                        &error,
                    )
                    .with_active_verb(snapshot.active_verb);
                    refuse_busy_nonblocking(&stream, refusal);
                    return;
                }
            };
            let job: PooledJob = Box::new(move || {
                intake_frame(
                    Arc::new(response_stream),
                    bytes,
                    deadline,
                    handler,
                    &admission,
                    &state,
                )
            });
            (stream, "frame", job)
        }
        FinishedFrame::Failed(stream, failure) => {
            let response_stream = match stream.try_clone() {
                Ok(stream) => stream,
                Err(_) => return,
            };
            let job: PooledJob = Box::new(move || handle_frame_failure(&response_stream, failure));
            (stream, "frame-error", job)
        }
    };
    if let Err((cause, _job)) = admission.ingress().submit_labeled(label, job) {
        let snapshot = admission.ingress().snapshot();
        refuse_busy_nonblocking(
            &stream,
            refusal_for_snapshot(admission.ingress().name(), snapshot, cause),
        );
    }
}

fn configure(stream: &UnixStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(FRAME_TIMEOUT))
}

struct BoundedFrameWriter<'a> {
    stream: &'a UnixStream,
    ends: Instant,
    request_deadline: Option<&'a QueryDeadline>,
}

impl std::io::Write for BoundedFrameWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let remaining = self.ends.saturating_duration_since(Instant::now());
        let remaining = self
            .request_deadline
            .map_or(remaining, |deadline| remaining.min(deadline.remaining()));
        if remaining.is_zero() {
            return Err(std::io::Error::from(ErrorKind::TimedOut));
        }
        self.stream.set_write_timeout(Some(remaining))?;
        let mut writer = self.stream;
        let result = std::io::Write::write(&mut writer, bytes);
        if Instant::now() >= self.ends || self.request_deadline.is_some_and(QueryDeadline::expired)
        {
            return Err(std::io::Error::from(ErrorKind::TimedOut));
        }
        result
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if Instant::now() >= self.ends || self.request_deadline.is_some_and(QueryDeadline::expired)
        {
            return Err(std::io::Error::from(ErrorKind::TimedOut));
        }
        let mut writer = self.stream;
        std::io::Write::flush(&mut writer)
    }
}

fn write_bounded_reply(
    stream: &UnixStream,
    reply: &DaemonReply,
    budget: Duration,
    request_deadline: Option<&QueryDeadline>,
) -> Result<()> {
    let mut writer = BoundedFrameWriter {
        stream,
        ends: Instant::now() + budget,
        request_deadline,
    };
    protocol::write_reply(&mut writer, reply)
}

/// Writes one small overload reply without holding the accept loop on a peer.
/// The write side then closes, so the peer reads a complete reply.
fn refuse_busy_nonblocking(stream: &UnixStream, refusal: AdmissionRefusal<'_>) {
    refusal.log();
    let reply = DaemonReply::Failure {
        message: refusal.message(),
    };
    let mut encoded = Vec::with_capacity(256);
    if protocol::write_reply(&mut encoded, &reply).is_err() || stream.set_nonblocking(true).is_err()
    {
        return;
    }
    let mut writer = stream;
    let _ = std::io::Write::write(&mut writer, &encoded);
    let _ = stream.shutdown(Shutdown::Write);
}

fn write_refusal(stream: &UnixStream, refusal: AdmissionRefusal<'_>) {
    refusal.log();
    let reply = DaemonReply::Failure {
        message: refusal.message(),
    };
    log_reply_failure(
        "daemon refusal response failed",
        write_bounded_reply(stream, &reply, FRAME_TIMEOUT, None),
    );
}

fn write_refusal_shared(stream: &UnixStream, refusal: AdmissionRefusal<'_>) {
    write_refusal(stream, refusal);
}

fn write_result(stream: &UnixStream, result: Result<String>) {
    let budget = result
        .as_ref()
        .err()
        .filter(|error| crate::coded_error::ErrorCode::TimedOut.is_code_of(error))
        .map_or(FRAME_TIMEOUT, |_| EXPIRED_REPLY_GRACE);
    let reply = DaemonReply::from_result(result);
    log_reply_failure(
        "daemon response failed",
        write_bounded_reply(stream, &reply, budget, None),
    );
}

/// Preserves a downstream reply timeout while making this expired bounded
/// request's own deadline the leading terminal failure.
fn normalize_owned_query_timeout(
    class: RequestClass,
    result: Result<String>,
    deadline: &QueryDeadline,
) -> Result<String> {
    let expired_own_budget = bounded_query_class(class)
        && deadline.time_expired()
        && result
            .as_ref()
            .err()
            .is_some_and(|error| super::DaemonReplyTimeout::from_error(error).is_some());
    if expired_own_budget {
        result.map_err(|error| {
            error.with_coded_context(|| coded!(TimedOut, "query deadline expired"))
        })
    } else {
        result
    }
}

fn log_reply_failure(prefix: &str, result: Result<()>) {
    if let Err(error) = result
        && !peer_disconnected(&error)
    {
        eprintln!("trufflepig: {prefix}: {error:#}");
    }
}

fn peer_disconnected(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                ErrorKind::BrokenPipe
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::NotConnected
                    | ErrorKind::UnexpectedEof
            )
        })
    })
}

fn refusal_for_snapshot<'a>(
    pool: &'a str,
    snapshot: super::pool::PoolSnapshot,
    cause: PoolSubmitCause,
) -> AdmissionRefusal<'a> {
    let cause = match cause {
        PoolSubmitCause::Full => RefusalCause::QueueFull,
        PoolSubmitCause::Closed => RefusalCause::PoolClosed,
    };
    AdmissionRefusal::new(pool, snapshot.queue_depth, snapshot.queue_capacity, cause)
        .with_active_verb(snapshot.active_verb)
}

/// Runs one request, turning a panic into an `internal_error` reply.
fn guarded_request(handler: &impl DaemonHandler, request: AcceptedRequest) -> Result<String> {
    catch_unwind(AssertUnwindSafe(|| handler.request(request))).unwrap_or_else(|panic| {
        anyhow::bail!(coded!(InternalError, "{}", panic_message(panic.as_ref())))
    })
}

/// The text a panic was raised with.
pub(super) fn panic_message(panic: &(dyn Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic without a message".to_owned())
}

/// A peer that closed before sending a request (a liveness probe) gets no reply.
fn closed_before_request(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == ErrorKind::UnexpectedEof)
    })
}

fn handle_frame_failure(stream: &UnixStream, failure: FrameFailure) {
    match failure {
        FrameFailure::TimedOut(_error) => {
            if configure(stream).is_err() {
                return;
            }
            let reply = DaemonReply::Failure {
                message: coded!(TimedOut, "phase=reading_frame").to_string(),
            };
            log_reply_failure(
                "daemon response failed",
                write_bounded_reply(stream, &reply, EXPIRED_REPLY_GRACE, None),
            );
        }
        FrameFailure::InvalidLength(length) => {
            if configure(stream).is_err() {
                return;
            }
            write_result(
                stream,
                Err(
                    anyhow::anyhow!("daemon frame exceeds {} bytes", protocol::REQUEST_LIMIT)
                        .context(format!("invalid frame length {length}")),
                ),
            );
        }
        FrameFailure::Io(error) => {
            let error = anyhow::Error::new(error).context("read daemon frame");
            if !closed_before_request(&error) {
                if configure(stream).is_err() {
                    return;
                }
                write_result(stream, Err(error));
            }
        }
    }
}

fn intake_frame<H: DaemonHandler>(
    stream: Arc<UnixStream>,
    bytes: Vec<u8>,
    deadline: QueryDeadline,
    handler: Arc<H>,
    admission: &RequestAdmission,
    state: &ServerState,
) {
    if let Err(error) = configure(&stream) {
        write_result(&stream, Err(error.into()));
        return;
    }
    let deadline = deadline.with_peer(Arc::clone(&stream));
    if deadline.expired() {
        write_result(
            &stream,
            Err(anyhow::anyhow!(coded!(
                TimedOut,
                "query deadline expired before decode"
            ))),
        );
        return;
    }
    let request = protocol::read_request(&mut std::io::Cursor::new(bytes));
    match request {
        Ok(DaemonRequest::Arguments { context, args }) => {
            if deadline.expired() {
                write_result(
                    &stream,
                    Err(anyhow::anyhow!(coded!(
                        TimedOut,
                        "query deadline expired before classification"
                    ))),
                );
                return;
            }
            let classified = match crate::cli::classify_daemon_request(&args) {
                Ok(classified) => classified,
                Err(error) => {
                    write_result(&stream, Err(error));
                    return;
                }
            };
            if deadline.expired() {
                write_result(
                    &stream,
                    Err(anyhow::anyhow!(coded!(
                        TimedOut,
                        "query deadline expired during classification"
                    ))),
                );
                return;
            }
            let original_class = classified.class;
            let (_, original_pool) = admission.execution(original_class);
            let admitted = match admission.admit(classified, args) {
                Ok(AdmissionDecision::Admitted(admitted)) => admitted,
                Ok(AdmissionDecision::RootLimit(limit)) => {
                    let snapshot = original_pool.snapshot();
                    let refusal = AdmissionRefusal::new(
                        original_pool.name(),
                        snapshot.queue_depth,
                        snapshot.queue_capacity,
                        RefusalCause::RootLimit,
                    )
                    .with_active_verb(snapshot.active_verb)
                    .with_root_limit(limit.outstanding, limit.limit);
                    write_refusal_shared(&stream, refusal);
                    return;
                }
                Err(error) => {
                    write_result(&stream, Err(error));
                    return;
                }
            };
            if deadline.expired() {
                write_result(
                    &stream,
                    Err(anyhow::anyhow!(coded!(
                        TimedOut,
                        "query deadline expired during admission"
                    ))),
                );
                return;
            }
            let class = admitted.class;
            let args = admitted.args;
            let permit = admitted.permit;
            let fallback = admitted.fallback;
            let (_, pool) = admission.execution(class);
            let job: PooledJob = {
                let (handler, stream) = (Arc::clone(&handler), Arc::clone(&stream));
                Box::new(move || {
                    let _permit = permit;
                    run_connection_request(
                        stream, deadline, class, context, args, handler, fallback,
                    )
                })
            };
            if let Err((cause, job)) = pool.submit_labeled(class.verb, job) {
                drop(job);
                let snapshot = pool.snapshot();
                write_refusal_shared(&stream, refusal_for_snapshot(pool.name(), snapshot, cause));
            }
        }
        Ok(DaemonRequest::Stop) => {
            state.stop();
            let reply = DaemonReply::Success {
                output: "{\"status\":\"stopped\"}".to_owned(),
            };
            log_reply_failure(
                "daemon response failed",
                write_bounded_reply(&stream, &reply, FRAME_TIMEOUT, None),
            );
        }
        Err(error) if closed_before_request(&error) => {}
        Err(error) => write_result(&stream, Err(error)),
    }
}

fn run_connection_request<H: DaemonHandler>(
    stream: Arc<UnixStream>,
    deadline: QueryDeadline,
    class: RequestClass,
    context: RequestContext,
    args: Vec<String>,
    handler: Arc<H>,
    fallback: Option<SemanticAdmissionFallback>,
) {
    let result = if deadline.expired() {
        Err(anyhow::anyhow!(coded!(
            TimedOut,
            "query deadline expired or peer closed"
        )))
    } else {
        guarded_request(
            &*handler,
            AcceptedRequest {
                context,
                args,
                deadline: deadline.clone(),
            },
        )
    };
    let result = apply_semantic_fallback(result, fallback);
    let result = if result.is_ok() && discards_late_result(class, &deadline) {
        Err(anyhow::anyhow!(coded!(
            TimedOut,
            "query deadline expired or peer closed"
        )))
    } else {
        result
    };
    let result = normalize_owned_query_timeout(class, result, &deadline);
    write_result(&stream, result);
}

fn apply_semantic_fallback(
    result: Result<String>,
    fallback: Option<SemanticAdmissionFallback>,
) -> Result<String> {
    match (fallback, result) {
        (Some(fallback), Ok(output)) => fallback.annotate(&output),
        (_, result) => result,
    }
}

fn bounded_query_class(class: RequestClass) -> bool {
    class.lane == RequestLane::Regex
        || class.lane == RequestLane::Semantic
        || (class.lane == RequestLane::Query
            && matches!(
                class.verb,
                "search"
                    | "show"
                    | "more"
                    | "ctx"
                    | "refs"
                    | "map"
                    | "status"
                    | "doctor"
                    | "hist"
                    | "since"
                    | "diff"
                    | "blame"
                    | "hist-status"
                    | "session"
                    | "audit"
                    | "semantic"
            ))
}

/// Whether a successful result that finished after its deadline becomes `timed_out`.
/// Only handlers that observe the deadline while working discard late results; the
/// others finish regardless, and their results still fit the router and client budgets.
/// A withdrawn request (peer or spool claim gone) never gets its late result.
fn discards_late_result(class: RequestClass, deadline: &QueryDeadline) -> bool {
    bounded_query_class(class)
        && deadline.expired()
        && (deadline_observing_class(class) || deadline.cancelled())
}

/// Bounded classes whose handlers take the query deadline cooperatively
/// (`docs/runtime-contract.md`). History, doctor, session, audit, and
/// `semantic status` run without it.
fn deadline_observing_class(class: RequestClass) -> bool {
    class.lane == RequestLane::Regex
        || class.lane == RequestLane::Semantic
        || (class.lane == RequestLane::Query
            && matches!(
                class.verb,
                "search" | "show" | "more" | "ctx" | "refs" | "map" | "status"
            ))
}

/// Claims each pending spooled request, classifies it before admission, and
/// checks client withdrawal before a queued request reaches the handler.
pub(super) fn dispatch_spooled<H: DaemonHandler>(
    spool: &mut SpoolServer,
    handler: &Arc<H>,
    admission: &Arc<RequestAdmission>,
) {
    for claimed in spool.claim() {
        if !claimed.is_claimed() {
            continue;
        }
        let deadline = claimed.deadline(admission.accepted_budget());
        if deadline.time_expired() {
            claimed.answer_result(Err(anyhow::anyhow!(coded!(
                TimedOut,
                "spooled request expired before classification"
            ))));
            continue;
        }
        let (context, args) = match claimed.arguments() {
            Ok(arguments) => arguments,
            Err(error) => {
                if claimed.is_claimed() {
                    claimed.answer_result(Err(error));
                }
                continue;
            }
        };
        if !claimed.is_claimed() {
            continue;
        }
        if deadline.time_expired() {
            claimed.answer_result(Err(anyhow::anyhow!(coded!(
                TimedOut,
                "spooled request expired before classification"
            ))));
            continue;
        }
        let classified = match crate::cli::classify_daemon_request(&args) {
            Ok(classified) => classified,
            Err(error) => {
                claimed.answer_result(Err(error));
                continue;
            }
        };
        if !claimed.is_claimed() {
            continue;
        }
        if deadline.time_expired() {
            claimed.answer_result(Err(anyhow::anyhow!(coded!(
                TimedOut,
                "spooled request expired during classification"
            ))));
            continue;
        }
        let original_class = classified.class;
        let (_, original_pool) = admission.execution(original_class);
        let admitted = match admission.admit(classified, args) {
            Ok(AdmissionDecision::Admitted(admitted)) => admitted,
            Ok(AdmissionDecision::RootLimit(limit)) => {
                let snapshot = original_pool.snapshot();
                let refusal = AdmissionRefusal::new(
                    original_pool.name(),
                    snapshot.queue_depth,
                    snapshot.queue_capacity,
                    RefusalCause::RootLimit,
                )
                .with_active_verb(snapshot.active_verb)
                .with_root_limit(limit.outstanding, limit.limit);
                refusal.log();
                claimed.answer_result(Err(refusal.error().into()));
                continue;
            }
            Err(error) => {
                claimed.answer_result(Err(error));
                continue;
            }
        };
        if !claimed.is_claimed() {
            continue;
        }
        if deadline.time_expired() {
            claimed.answer_result(Err(anyhow::anyhow!(coded!(
                TimedOut,
                "spooled request expired during admission"
            ))));
            continue;
        }
        let class = admitted.class;
        let args = admitted.args;
        let permit = admitted.permit;
        let fallback = admitted.fallback;
        let (_, pool) = admission.execution(class);
        let claimed = Arc::new(Mutex::new(Some(claimed)));
        let job: PooledJob = {
            let (handler, claimed) = (Arc::clone(handler), Arc::clone(&claimed));
            Box::new(move || {
                let _permit = permit;
                if let Some(claimed) = take_claim(&claimed) {
                    if !claimed.is_claimed() {
                        return;
                    }
                    let result = if deadline.expired() {
                        Err(anyhow::anyhow!(coded!(
                            TimedOut,
                            "spooled request expired before dispatch"
                        )))
                    } else {
                        guarded_request(
                            &*handler,
                            AcceptedRequest {
                                context,
                                args,
                                deadline: deadline.clone(),
                            },
                        )
                    };
                    let result = apply_semantic_fallback(result, fallback);
                    let result = if result.is_ok() && discards_late_result(class, &deadline) {
                        Err(anyhow::anyhow!(coded!(TimedOut, "query deadline expired")))
                    } else {
                        result
                    };
                    let result = normalize_owned_query_timeout(class, result, &deadline);
                    claimed.answer_result(result);
                }
            })
        };
        if let Err((cause, job)) = pool.submit_labeled(class.verb, job) {
            drop(job);
            if let Some(claimed) = take_claim(&claimed)
                && claimed.is_claimed()
            {
                let refusal = refusal_for_snapshot(pool.name(), pool.snapshot(), cause);
                refusal.log();
                claimed.answer_result(Err(refusal.error().into()));
            }
        }
    }
}

fn take_claim(claimed: &Mutex<Option<ClaimedSpoolRequest>>) -> Option<ClaimedSpoolRequest> {
    claimed.lock().ok().and_then(|mut claimed| claimed.take())
}

#[cfg(test)]
mod tests;
