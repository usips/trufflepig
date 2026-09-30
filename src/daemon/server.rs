//! Concurrent daemon serving: the calling thread only accepts, a [`RequestPool`]
//! reads, dispatches, and answers each connection, and the maintenance thread
//! (`reconciler.rs`) reconciles beside them. A full pool answers `daemon_busy`.
use super::DaemonSocket;
use super::deadline::QueryDeadline;
use super::pool::{PoolSize, PooledJob, RequestPool};
use super::protocol::{self, DaemonReply, DaemonRequest};
use super::reconciler::{self, Maintenance};
use super::spool::{ClaimedSpoolRequest, SpoolServer};
use crate::diagnostics::RequestContext;
use anyhow::{Context, Result};
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// Reply to a request refused because every worker is busy and the queue is full.
pub const DAEMON_BUSY: &str = "daemon_busy: retry";
/// Idle wait of the accept loop between polls of a nonblocking listener.
const ACCEPT_POLL: Duration = Duration::from_millis(10);
/// Time a request frame may take to arrive, and a reply to drain.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a stopping server waits for a reconcile in progress before returning.
const MAINTENANCE_STOP_WAIT: Duration = Duration::from_secs(1);

/// One accepted request; `deadline` starts when the connection is accepted.
pub struct AcceptedRequest {
    pub context: RequestContext,
    pub args: Vec<String>,
    pub deadline: QueryDeadline,
}

/// Daemon behavior shared by request workers and the maintenance thread.
pub trait DaemonHandler: Send + Sync + 'static {
    fn request(&self, request: AcceptedRequest) -> Result<String>;
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

/// How one daemon tier serves.
pub(super) struct ServeProfile<'a> {
    pub name: &'a str,
    pub watching: bool,
    pub pool: PoolSize,
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
    let pool = Arc::new(RequestPool::new(profile.name, profile.pool)?);
    let state = Arc::new(ServerState::default());
    let (finished, maintenance_done) = mpsc::channel::<()>();
    {
        let (handler, pool, state) = (Arc::clone(&handler), Arc::clone(&pool), Arc::clone(&state));
        let maintenance = Maintenance {
            root,
            cache: cache.clone(),
            watching: profile.watching,
            spool,
        };
        std::thread::Builder::new()
            .name(format!("{}-reconciler", profile.name))
            .spawn(move || {
                reconciler::run(maintenance, &handler, &pool, &state);
                drop(finished);
            })?;
    }
    let accepted = accept_until_stopped(&socket, &handler, &pool, &state);
    // A reconcile in progress may outlive the server; its writer lease keeps the
    // index coherent and the process exit ends it.
    let _ = maintenance_done.recv_timeout(MAINTENANCE_STOP_WAIT);
    drop(socket);
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
    pool: &Arc<RequestPool>,
    state: &Arc<ServerState>,
) -> Result<()> {
    while !state.stopping() {
        match socket.listener.accept() {
            Ok((stream, _)) => {
                let deadline = QueryDeadline::start();
                // Out of descriptors is as busy as a full queue.
                let Ok(worker_stream) = stream.try_clone() else {
                    refuse_busy(stream);
                    continue;
                };
                let job: PooledJob = {
                    let (handler, state) = (Arc::clone(handler), Arc::clone(state));
                    Box::new(move || answer_connection(worker_stream, deadline, &*handler, &state))
                };
                if pool.submit(job).is_err() {
                    refuse_busy(stream);
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_POLL),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("accept daemon request"),
        }
    }
    Ok(())
}

fn configure(stream: &UnixStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(FRAME_TIMEOUT))?;
    stream.set_write_timeout(Some(FRAME_TIMEOUT))
}

/// Answers without reading: the client's request frame is already buffered.
fn refuse_busy(mut stream: UnixStream) {
    let reply = DaemonReply::Failure {
        message: DAEMON_BUSY.to_owned(),
    };
    if configure(&stream).is_ok() {
        let _ = protocol::write_reply(&mut stream, &reply);
    }
}

fn answer_connection(
    mut stream: UnixStream,
    deadline: QueryDeadline,
    handler: &impl DaemonHandler,
    state: &ServerState,
) {
    let request = configure(&stream)
        .map_err(anyhow::Error::from)
        .and_then(|()| protocol::read_request(&mut stream));
    let result = match request {
        Ok(DaemonRequest::Arguments { context, args }) => handler.request(AcceptedRequest {
            context,
            args,
            deadline,
        }),
        Ok(DaemonRequest::Stop) => {
            state.stop();
            Ok("{\"status\":\"stopped\"}".to_owned())
        }
        Err(error) => Err(error),
    };
    if let Err(error) = protocol::write_reply(&mut stream, &DaemonReply::from_result(result)) {
        eprintln!("trufflepig: daemon response failed: {error:#}");
    }
}

/// Claims each pending spooled request and answers it on the pool.
pub(super) fn dispatch_spooled<H: DaemonHandler>(
    spool: &mut SpoolServer,
    handler: &Arc<H>,
    pool: &Arc<RequestPool>,
) {
    for claimed in spool.claim() {
        let claimed = Arc::new(Mutex::new(Some(claimed)));
        let job: PooledJob = {
            let (handler, claimed) = (Arc::clone(handler), Arc::clone(&claimed));
            Box::new(move || {
                if let Some(claimed) = take_claim(&claimed) {
                    let deadline = QueryDeadline::start();
                    claimed.answer(|context, args| {
                        handler.request(AcceptedRequest {
                            context,
                            args,
                            deadline,
                        })
                    });
                }
            })
        };
        if pool.submit(job).is_err()
            && let Some(claimed) = take_claim(&claimed)
        {
            claimed.refuse(DAEMON_BUSY);
        }
    }
}

fn take_claim(claimed: &Mutex<Option<ClaimedSpoolRequest>>) -> Option<ClaimedSpoolRequest> {
    claimed.lock().ok().and_then(|mut claimed| claimed.take())
}

#[cfg(test)]
mod tests;
