//! Loopback board HTTP service, with bounded workers and independent query readers.
pub(crate) mod event_stream;
pub(crate) mod http_wire;
pub(crate) mod plan_markup;
mod reader_pool;
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
    io::Write,
    net::{Shutdown, SocketAddr, TcpListener},
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
const PUBLIC_SCRIPT: &str = include_str!("board_web/assets/app.js");
const PUBLIC_STYLE: &str = include_str!("board_web/assets/app.css");
const PUBLIC_DOM: &str = include_str!("board_web/assets/board_dom.js");
const PUBLIC_VIEWS: &str = include_str!("board_web/assets/board_views.js");
const PUBLIC_DETAILS: &str = include_str!("board_web/assets/board_details.js");
const PUBLIC_STREAM: &str = include_str!("board_web/assets/board_stream.js");
const PUBLIC_FEEDBACK: &str = include_str!("board_web/assets/board_feedback.js");
const PUBLIC_READER: &str = include_str!("board_web/assets/board_reader.js");
const PUBLIC_ENTRIES: &str = include_str!("board_web/assets/board_entries.js");

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
}

pub(crate) struct WebState {
    store: Arc<WebStore>,
    guard: WebGuard,
    streams: EventStreams,
}

impl BoardWebServer {
    pub(crate) fn bind(address: SocketAddr) -> Result<Self> {
        let (listener, guard) = WebGuard::bind(address)?;
        let store = Arc::new(WebStore::open(BoardConfigCache::default())?);
        let sequence_store = Arc::clone(&store);
        let poller = SequencePoller::start(Arc::new(move || {
            let expires = Instant::now() + http_wire::REQUEST_TIMEOUT;
            let config = sequence_store.config(expires)?;
            sequence_store
                .readers
                .with_reader(&config, expires, |reader| reader.max_seq())
        }))?;
        let feed_store = Arc::clone(&store);
        let streams = EventStreams::new(
            Arc::new(move |after, plan, limit| {
                let expires = Instant::now() + http_wire::REQUEST_TIMEOUT;
                let config = feed_store.config(expires)?;
                let (latest, events) =
                    feed_store.readers.with_reader(&config, expires, |reader| {
                        reader.read_event_batch(after, plan, limit)
                    })?;
                Ok(ReplayBatch { latest, events })
            }),
            poller.handle(),
        );
        let config = store.config(Instant::now() + http_wire::REQUEST_TIMEOUT)?;
        web_endpoint::publish(&store.runtime, listener.local_addr()?, &config.db_path)?;
        Ok(Self {
            listener,
            state: Arc::new(WebState {
                store,
                guard,
                streams,
            }),
            _poller: poller,
        })
    }

    pub(crate) fn bootstrap_url(&self) -> String {
        self.state.guard.bootstrap_url()
    }

    pub(crate) fn run(self) -> Result<()> {
        let pool = RequestPool::new("board-http", WEB_POOL)?;
        let body = br#"{"error":{"code":"daemon_busy","message":"board web queue is full"}}"#;
        let busy = http_wire::unavailable_response(1, body);
        for accepted in self.listener.incoming() {
            let stream = accepted?;
            let accepted_at = Instant::now();
            let refusal = stream.try_clone().ok();
            let state = Arc::clone(&self.state);
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
}

/// Foreground service; the bootstrap fragment is printed only to the starting terminal.
pub fn serve(address: SocketAddr) -> Result<()> {
    let server = BoardWebServer::bind(address)?;
    println!("board web: {}", server.bootstrap_url());
    server.run()
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
