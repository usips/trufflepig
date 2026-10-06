//! Listener bind and the bounded connection-accept loop.
use super::super::board_config::BoardConfigCache;
use super::event_stream::SequencePoller;
use super::{
    BoardWebState, WebStore, bootstrap_line, http_wire, open_stream_feed,
    published_endpoint::{PublishedEndpoint, remove_published_endpoint},
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
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const WEB_POOL: PoolSize = PoolSize {
    workers: 8,
    queue: 64,
};

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
        // Record and publish under one lock: a signal landing here makes
        // the waiter's cleanup wait for the completed descriptor, then
        // remove it, so no stale board-web.json can survive the signal.
        published.publish(
            endpoint.path().to_owned(),
            bound,
            &store.runtime,
            &config.db_path,
        )?;
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
    let published = Arc::new(PublishedEndpoint::default());
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

/// Admits one drain thread while fewer than `limit` are running.
pub(super) fn try_admit_drain(active: &AtomicUsize, limit: usize) -> bool {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < limit).then_some(active + 1)
        })
        .is_ok()
}

/// Drain-thread counters for one serve loop; the loop owns them, so a
/// restart or a concurrent test never shares admission state. Drain
/// threads hold an `Arc` clone to release their slot on the way out.
#[derive(Default)]
pub(super) struct RefusalDrains {
    active: AtomicUsize,
    #[cfg(test)]
    max: AtomicUsize,
}

impl RefusalDrains {
    /// Admits one drain thread while fewer than the limit are running.
    fn admit(&self) -> bool {
        let admitted = try_admit_drain(&self.active, REFUSAL_DRAIN_LIMIT);
        #[cfg(test)]
        if admitted {
            self.max
                .fetch_max(self.active.load(Ordering::Acquire), Ordering::AcqRel);
        }
        admitted
    }

    fn release(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }

    /// Highest concurrent drain count observed on these counters.
    #[cfg(test)]
    pub(super) fn max(&self) -> usize {
        self.max.load(Ordering::Acquire)
    }
}

struct DrainGuard(Arc<RefusalDrains>);

impl Drop for DrainGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Refuses a connection the pool cannot take: one nonblocking busy write,
/// then a short-lived thread drains unread input after shutting down the
/// write side, so the client reads the reply instead of a reset. The
/// accept loop never blocks on a refused connection.
pub(super) fn refuse_queue_full(mut refusal: TcpStream, busy: &[u8], drains: &Arc<RefusalDrains>) {
    // One nonblocking attempt never holds up accepting the next connection.
    if refusal.set_nonblocking(true).is_ok() {
        let _ = refusal.write(busy);
    }
    // Past the cap the socket drops here: an immediate close with
    // the busy bytes already written, and no drain thread at all.
    if !drains.admit() {
        return;
    }
    // A failed spawn drops the socket, as an immediate close would.
    let spawned = std::thread::Builder::new()
        .name("board-refusal-drain".into())
        .spawn({
            let drains = Arc::clone(drains);
            move || {
                let _guard = DrainGuard(drains);
                let _ = refusal.set_nonblocking(false);
                http_wire::close_after_error(&mut refusal);
            }
        });
    if spawned.is_err() {
        drains.release();
    }
}

pub(super) fn serve_connections(
    incoming: impl IntoIterator<Item = io::Result<TcpStream>>,
    state: &Arc<BoardWebState>,
) -> Result<()> {
    let pool = RequestPool::new("board-http", WEB_POOL)?;
    let busy = http_wire::unavailable_response(1, QUEUE_FULL_BODY);
    let drains = Arc::new(RefusalDrains::default());
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
                refuse_queue_full(refusal, &busy, &drains);
            }
        }
    }
}
