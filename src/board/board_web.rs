//! Loopback board HTTP service, with bounded workers and independent query readers.
pub(crate) mod event_stream;
pub(crate) mod http_wire;
pub(crate) mod plan_markup;
mod reader_pool;
mod serve_lock;
mod signal_shutdown;
#[cfg(test)]
mod tests;
mod web_endpoint;
pub(crate) mod web_guard;
mod web_ops;
mod web_routes;

use super::{
    board_backend::BoardBackend,
    board_config::{BoardConfig, BoardConfigCache},
    board_protocol::{BoardError, BoardErrorCode},
    local_board::LocalBoard,
};
use crate::daemon::{PoolSize, RequestPool};
use anyhow::{Context, Result};
use event_stream::{EventStreams, ReplayBatch, SequencePoller};
use reader_pool::ReaderPool;
use std::{
    io::{self, IsTerminal, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::{Duration, Instant},
};
use web_guard::WebGuard;

const WEB_POOL: PoolSize = PoolSize {
    workers: 8,
    queue: 64,
};
const PUBLIC_SHELL: &str = include_str!("board_web/assets/index.html");
const PUBLIC_MAIN: &str = include_str!("board_web/assets/board_web_main.js");
const PUBLIC_STYLE: &str = include_str!("board_web/assets/board_web.css");
const PUBLIC_DOM: &str = include_str!("board_web/assets/board_dom.js");
const PUBLIC_VIEWS: &str = include_str!("board_web/assets/board_views.js");
const PUBLIC_PAGES: &str = include_str!("board_web/assets/board_pages.js");
const PUBLIC_STREAM: &str = include_str!("board_web/assets/board_stream.js");
const PUBLIC_TRIAGE: &str = include_str!("board_web/assets/feedback_triage.js");
const PUBLIC_READER: &str = include_str!("board_web/assets/board_reader.js");
const PUBLIC_ENTRIES: &str = include_str!("board_web/assets/board_entries.js");
const PUBLIC_TOKEN: &str = include_str!("board_web/assets/board_web_token.js");

pub(crate) struct WebStore {
    config: Mutex<BoardConfigCache>,
    writer: Mutex<Option<LocalBoard>>,
    readers: ReaderPool,
    runtime: PathBuf,
}

impl WebStore {
    fn open(cache: BoardConfigCache) -> Result<Self> {
        let runtime =
            crate::system::dir().context("board_unavailable: no router runtime directory")?;
        Self::open_at(cache, runtime)
    }

    fn open_at(mut cache: BoardConfigCache, runtime: PathBuf) -> Result<Self> {
        let config = cache.get(Instant::now())?;
        crate::system::validate_board_database(&runtime, &config.db_path)?;
        web_ops::check_router_identity(
            &runtime,
            &config.db_path,
            Instant::now() + http_wire::REQUEST_TIMEOUT,
        )?;
        let writer = LocalBoard::open(&config)?;
        let readers = ReaderPool::new(&config)?;
        Ok(Self {
            config: Mutex::new(cache),
            writer: Mutex::new(Some(writer)),
            readers,
            runtime,
        })
    }

    fn config(&self, expires: Instant) -> Result<BoardConfig, BoardError> {
        let config = lock_until(&self.config, expires)?.get(Instant::now())?;
        crate::system::validate_board_database(&self.runtime, &config.db_path)
            .map_err(BoardError::from)?;
        Ok(config)
    }

    fn with_writer<T>(
        &self,
        config: &BoardConfig,
        expires: Instant,
        work: impl FnOnce(&mut LocalBoard) -> Result<T, BoardError>,
    ) -> Result<T, BoardError> {
        let mut slot = lock_until(&self.writer, expires)?;
        if self.writer.is_poisoned() {
            // A panicked transaction must never lend its connection to another request.
            slot.take();
            self.writer.clear_poison();
        }
        if slot.is_none() {
            *slot = Some(LocalBoard::open_with_timeout(
                config,
                expires.saturating_duration_since(Instant::now()),
            )?);
        }
        if Instant::now() >= expires {
            return Err(deadline_error());
        }
        let writer = slot.as_mut().expect("writer slot was opened");
        writer.set_busy_timeout(expires.saturating_duration_since(Instant::now()))?;
        writer.set_claim_ttl(Duration::from_secs(
            config.claim_ttl_seconds().cast_unsigned(),
        ))?;
        work(writer)
    }
}

pub(crate) struct BoardWebServer {
    listener: TcpListener,
    state: Arc<WebState>,
    _poller: SequencePoller,
    endpoint: web_endpoint::EndpointGuard,
    _serve_lock: serve_lock::ServeLock,
}

pub(crate) struct WebState {
    store: Arc<WebStore>,
    guard: WebGuard,
    streams: EventStreams,
    ingest: web_ops::IngestFlight,
}

/// The event-stream feed reads through one dedicated query-only connection
/// outside the reader pool: poller ticks and ring fills never contend with
/// request reads, and streams never check out a pooled connection.
struct StreamFeed {
    store: Arc<WebStore>,
    board: Mutex<LocalBoard>,
}

impl StreamFeed {
    fn open(store: &Arc<WebStore>) -> Result<Self, BoardError> {
        let config = store.config(Instant::now() + http_wire::REQUEST_TIMEOUT)?;
        let board = LocalBoard::open_read_with_timeout(&config, Duration::from_secs(5))?;
        Ok(Self {
            store: Arc::clone(store),
            board: Mutex::new(board),
        })
    }

    fn read<T>(
        &self,
        work: impl FnOnce(&mut LocalBoard) -> Result<T, BoardError>,
    ) -> Result<T, BoardError> {
        let expires = Instant::now() + http_wire::REQUEST_TIMEOUT;
        let config = self.store.config(expires)?;
        let mut board = self
            .board
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        board.set_busy_timeout(expires.saturating_duration_since(Instant::now()))?;
        board.set_claim_ttl(Duration::from_secs(
            config.claim_ttl_seconds().cast_unsigned(),
        ))?;
        work(&mut board)
    }
}

/// The poller and the shared ring share the one dedicated feed connection.
fn open_stream_feed(store: &Arc<WebStore>) -> Result<(SequencePoller, EventStreams)> {
    let feed = Arc::new(StreamFeed::open(store)?);
    let sequence_feed = Arc::clone(&feed);
    let poller = SequencePoller::start(Arc::new(move || {
        sequence_feed.read(|board| board.max_seq())
    }))?;
    let streams = EventStreams::new(
        Arc::new(move |after, plan, limit| {
            feed.read(|board| board.read_event_batch(after, plan, limit))
                .map(|(latest, events)| ReplayBatch { latest, events })
        }),
        poller.handle(),
    );
    Ok((poller, streams))
}

impl BoardWebServer {
    pub(crate) fn bind(address: SocketAddr) -> Result<Self> {
        let runtime =
            crate::system::dir().context("board_unavailable: no router runtime directory")?;
        let serve_lock = serve_lock::acquire_at(&runtime)?;
        let listener = web_endpoint::bind_listener(&runtime, address)?;
        let (listener, guard) = WebGuard::with_listener(listener)?;
        let store = Arc::new(WebStore::open(BoardConfigCache::default())?);
        let (poller, streams) = open_stream_feed(&store)?;
        let config = store.config(Instant::now() + http_wire::REQUEST_TIMEOUT)?;
        web_endpoint::publish(&store.runtime, listener.local_addr()?, &config.db_path)?;
        let endpoint = web_endpoint::EndpointGuard::arm(&store.runtime, listener.local_addr()?);
        Ok(Self {
            listener,
            state: Arc::new(WebState {
                store,
                guard,
                streams,
                ingest: web_ops::IngestFlight::default(),
            }),
            _poller: poller,
            endpoint,
            _serve_lock: serve_lock,
        })
    }

    pub(crate) fn run(self) -> Result<()> {
        serve_connections(self.listener.incoming(), &self.state)
    }
}

/// Transient accept failures (aborted handshakes, resource pressure) skip to
/// the next connection; anything else stops the server.
fn transient_accept_error(error: &io::Error) -> bool {
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

fn serve_connections(
    incoming: impl IntoIterator<Item = io::Result<TcpStream>>,
    state: &Arc<WebState>,
) -> Result<()> {
    let pool = RequestPool::new("board-http", WEB_POOL)?;
    let body = br#"{"error":{"code":"daemon_busy","message":"board web queue is full"}}"#;
    let busy = http_wire::unavailable_response(1, body);
    for accepted in incoming {
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
            if let Some(mut refusal) = refusal {
                // One nonblocking attempt never holds up accepting the next connection.
                if refusal.set_nonblocking(true).is_ok() {
                    let _ = refusal.write(&busy);
                }
                let _ = refusal.shutdown(Shutdown::Both);
            }
        }
    }
    Ok(())
}

/// Foreground service; the full bootstrap URL is printed only when stdout is
/// a terminal, never to pipes or the systemd journal.
pub fn serve(address: SocketAddr) -> Result<()> {
    signal_shutdown::block_termination()?;
    let server = BoardWebServer::bind(address)?;
    let descriptor = server.endpoint.path().to_owned();
    let address = server.endpoint.address();
    signal_shutdown::spawn_exit_waiter(move || {
        web_endpoint::remove_if_ours(&descriptor, address);
    })?;
    println!(
        "{}",
        bootstrap_line(&server.state.guard, std::io::stdout().is_terminal())
    );
    server.run()
}

/// The token-bearing URL is for the starting terminal only; anything else
/// gets the origin plus the command that prints the URL on demand.
fn bootstrap_line(guard: &WebGuard, is_tty: bool) -> String {
    if is_tty {
        format!("board web: {}", guard.bootstrap_url())
    } else {
        format!(
            "board web: {}; run `trufflepig board web` for the bootstrap URL",
            guard.origin()
        )
    }
}

/// Return the live listener's bootstrap URL, optionally opening a plan or entry.
pub fn link(target: Option<super::board_ids::BoardRef>) -> Result<String> {
    web_endpoint::link(target)
}

pub(crate) fn deadline_error() -> BoardError {
    BoardError::new(BoardErrorCode::DaemonBusy, "web request deadline expired")
}

pub(crate) fn lock_until<T>(
    mutex: &Mutex<T>,
    expires: Instant,
) -> Result<MutexGuard<'_, T>, BoardError> {
    loop {
        if Instant::now() >= expires {
            return Err(deadline_error());
        }
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(poison)) => return Ok(poison.into_inner()),
            Err(TryLockError::WouldBlock) => std::thread::sleep(
                Duration::from_millis(1).min(expires.saturating_duration_since(Instant::now())),
            ),
        }
    }
}
