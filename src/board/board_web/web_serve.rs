//! Listener bind and the bounded connection-accept loop.
use super::super::board_config::BoardConfigCache;
use super::event_stream::SequencePoller;
use super::{
    BoardWebState, WebStore, bootstrap_line, http_wire, open_stream_feed,
    serve_lock::{self, ServeLock},
    signal_shutdown,
    web_endpoint::{self, EndpointGuard},
    web_guard::WebGuard,
    web_ops, web_routes,
};
use crate::daemon::{PoolSize, RequestPool};
use anyhow::{Context, Result};
use std::{
    io::{self, IsTerminal, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const WEB_POOL: PoolSize = PoolSize {
    workers: 8,
    queue: 64,
};

/// Endpoint identity for the pre-bind signal waiter; bind fills it in
/// before publishing the descriptor file.
pub(crate) type PublishedEndpoint = OnceLock<(PathBuf, SocketAddr)>;

/// Removes the descriptor bind published, if any; a signal during early
/// bind finds nothing set and exits cleanly without cleanup.
pub(super) fn remove_published_endpoint(published: &PublishedEndpoint) {
    if let Some((path, address)) = published.get() {
        web_endpoint::remove_if_ours(path, *address);
    }
}

pub(crate) struct BoardWebServer {
    listener: TcpListener,
    state: Arc<BoardWebState>,
    _poller: SequencePoller,
    _endpoint: EndpointGuard,
    _serve_lock: ServeLock,
}

impl BoardWebServer {
    pub(crate) fn bind(address: SocketAddr, published: &PublishedEndpoint) -> Result<Self> {
        let runtime =
            crate::system::dir().context("board_unavailable: no router runtime directory")?;
        let serve_lock = serve_lock::acquire_at(&runtime)?;
        let listener = web_endpoint::bind_listener(&runtime, address)?;
        let (listener, guard) = WebGuard::with_listener(listener)?;
        let store = Arc::new(WebStore::open(BoardConfigCache::default())?);
        let (poller, streams) = open_stream_feed(&store)?;
        let config = store.config(Instant::now() + http_wire::REQUEST_TIMEOUT)?;
        let board_id = store.with_writer(
            &config,
            Instant::now() + http_wire::REQUEST_TIMEOUT,
            |writer| writer.board_uuid(),
        )?;
        let bound = listener.local_addr()?;
        let endpoint = EndpointGuard::arm(&store.runtime, bound);
        // Memory before file: a signal landing here removes at most a
        // stale descriptor, and never leaves a fresh one behind.
        let _ = published.set((endpoint.path().to_owned(), bound));
        web_endpoint::publish(&store.runtime, bound, &config.db_path)?;
        Ok(Self {
            listener,
            state: Arc::new(BoardWebState {
                store,
                guard,
                streams,
                ingest: web_ops::IngestFlight::default(),
                board_id,
            }),
            _poller: poller,
            _endpoint: endpoint,
            _serve_lock: serve_lock,
        })
    }

    pub(crate) fn run(self) -> Result<()> {
        serve_connections(self.listener.incoming(), &self.state)
    }
}

/// Foreground service; the full bootstrap URL is printed only when stdout is
/// a terminal, never to pipes or the systemd journal.
pub fn serve(address: SocketAddr) -> Result<()> {
    signal_shutdown::block_termination()?;
    // The waiter starts before bind: bind can wait on the router for a
    // migration, and a SIGTERM during that wait must still exit promptly.
    // Bind publishes the endpoint into the lock once it is known.
    let published: Arc<PublishedEndpoint> = Arc::new(OnceLock::new());
    let waiter_published = Arc::clone(&published);
    signal_shutdown::spawn_exit_waiter(move || {
        remove_published_endpoint(&waiter_published);
    })?;
    let server = BoardWebServer::bind(address, &published)?;
    println!(
        "{}",
        bootstrap_line(&server.state.guard, std::io::stdout().is_terminal())
    );
    server.run()
}

/// Transient accept failures (aborted handshakes, resource pressure) skip to
/// the next connection; anything else stops the server.
pub(super) fn transient_accept_error(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
    ) {
        return true;
    }
    matches!(
        error.raw_os_error(),
        Some(libc::ECONNABORTED | libc::ENFILE | libc::EMFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

pub(super) const QUEUE_FULL_BODY: &[u8] =
    br#"{"error":{"code":"daemon_busy","message":"board web queue is full"}}"#;

/// At most this many refusal-drain threads run at once; past the cap a
/// refusal closes at once instead of draining unread input.
pub(super) const REFUSAL_DRAIN_LIMIT: usize = 16;

static REFUSAL_DRAINS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static REFUSAL_DRAIN_MAX: AtomicUsize = AtomicUsize::new(0);

/// Admits one drain thread while fewer than `limit` are running.
pub(super) fn try_admit_drain(active: &AtomicUsize, limit: usize) -> bool {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < limit).then_some(active + 1)
        })
        .is_ok()
}

struct DrainGuard;

impl Drop for DrainGuard {
    fn drop(&mut self) {
        REFUSAL_DRAINS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Highest concurrent drain count observed since the last reset.
#[cfg(test)]
pub(super) fn refusal_drain_max() -> usize {
    REFUSAL_DRAIN_MAX.load(Ordering::Acquire)
}

/// Restarts the highest-concurrent-drain observation window.
#[cfg(test)]
pub(super) fn reset_refusal_drain_max() {
    REFUSAL_DRAIN_MAX.store(0, Ordering::Release);
}

/// Refuses a connection the pool cannot take: one nonblocking busy write,
/// then a short-lived thread drains unread input after shutting down the
/// write side, so the client reads the reply instead of a reset. The
/// accept loop never blocks on a refused connection.
pub(super) fn refuse_queue_full(mut refusal: TcpStream, busy: &[u8]) {
    // One nonblocking attempt never holds up accepting the next connection.
    if refusal.set_nonblocking(true).is_ok() {
        let _ = refusal.write(busy);
    }
    // Past the cap the socket drops here: an immediate close with
    // the busy bytes already written, and no drain thread at all.
    if !try_admit_drain(&REFUSAL_DRAINS, REFUSAL_DRAIN_LIMIT) {
        return;
    }
    #[cfg(test)]
    REFUSAL_DRAIN_MAX.fetch_max(REFUSAL_DRAINS.load(Ordering::Acquire), Ordering::AcqRel);
    // A failed spawn drops the socket, as an immediate close would.
    let spawned = std::thread::Builder::new()
        .name("board-refusal-drain".into())
        .spawn(move || {
            let _guard = DrainGuard;
            let _ = refusal.set_nonblocking(false);
            http_wire::close_after_error(&mut refusal);
        });
    if spawned.is_err() {
        REFUSAL_DRAINS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn serve_connections(
    incoming: impl IntoIterator<Item = io::Result<TcpStream>>,
    state: &Arc<BoardWebState>,
) -> Result<()> {
    let pool = RequestPool::new("board-http", WEB_POOL)?;
    let busy = http_wire::unavailable_response(1, QUEUE_FULL_BODY);
    let mut incoming = incoming.into_iter();
    loop {
        // A stopped ring never recovers in-process; fail so systemd
        // restarts instead of answering 503 Retry-After forever.
        if state.streams.ring_stopped() {
            return Err(anyhow::anyhow!("board_unavailable: event ring stopped"));
        }
        let Some(accepted) = incoming.next() else {
            return Ok(());
        };
        let stream = match accepted {
            Ok(stream) => stream,
            Err(error) if transient_accept_error(&error) => {
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENFILE | libc::EMFILE | libc::ENOBUFS | libc::ENOMEM)
                ) {
                    // Resource pressure persists until workers free descriptors.
                    std::thread::sleep(Duration::from_millis(10));
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let accepted_at = Instant::now();
        let refusal = stream.try_clone().ok();
        let state = Arc::clone(state);
        let job = Box::new(move || web_routes::handle(stream, accepted_at, &state));
        if let Err(job) = pool.submit(job) {
            drop(job);
            if let Some(refusal) = refusal {
                refuse_queue_full(refusal, &busy);
            }
        }
    }
}
