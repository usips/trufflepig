//! Loopback board HTTP service, with bounded workers and independent query readers.
mod board_web_secrets;
pub(crate) mod event_stream;
pub(crate) mod http_wire;
pub(crate) mod plan_markup;
mod published_endpoint;
mod reader_pool;
mod serve_lock;
mod signal_shutdown;
#[cfg(test)]
mod tests;
mod web_endpoint;
pub(crate) mod web_guard;
mod web_ops;
mod web_routes;
mod web_serve;

use super::{
    board_backend::BoardBackend,
    board_config::{BoardConfig, BoardConfigCache},
    board_projects::{ProjectResolver, ResolvedProject},
    board_protocol::{BoardError, BoardErrorCode},
    local_board::LocalBoard,
};
use anyhow::{Context, Result};
use event_stream::{EventStreams, ReplayBatch, SequencePoller};
use reader_pool::ReaderPool;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::{Duration, Instant},
};
use web_guard::WebGuard;
pub use web_serve::serve;

const PUBLIC_SHELL: &str = include_str!("board_web/assets/index.html");
const PUBLIC_MAIN: &str = include_str!("board_web/assets/board_web_main.js");
const PUBLIC_STYLE: &str = include_str!("board_web/assets/board_web.css");
const PUBLIC_DOM: &str = include_str!("board_web/assets/board_dom.js");
const PUBLIC_VIEWS: &str = include_str!("board_web/assets/board_views.js");
const PUBLIC_CARDS: &str = include_str!("board_web/assets/board_cards.js");
const PUBLIC_ROUTING: &str = include_str!("board_web/assets/board_routing.js");
const PUBLIC_RENDER_LOOP: &str = include_str!("board_web/assets/board_render_loop.js");
const PUBLIC_PAGES: &str = include_str!("board_web/assets/pages/board_pages.js");
const PUBLIC_PLAN_PAGE: &str = include_str!("board_web/assets/pages/plan_page.js");
const PUBLIC_DONE_PAGE: &str = include_str!("board_web/assets/pages/done_page.js");
const PUBLIC_PROPOSAL_PAGE: &str = include_str!("board_web/assets/pages/proposal_page.js");
const PUBLIC_STREAM: &str = include_str!("board_web/assets/stream/board_stream.js");
const PUBLIC_STREAM_ELECTION: &str = include_str!("board_web/assets/stream/stream_election.js");
const PUBLIC_STREAM_PARSE: &str = include_str!("board_web/assets/stream/stream_parse.js");
const PUBLIC_TRIAGE: &str = include_str!("board_web/assets/feedback_triage.js");
const PUBLIC_READER: &str = include_str!("board_web/assets/board_reader.js");
const PUBLIC_ENTRIES: &str = include_str!("board_web/assets/board_entries.js");
const PUBLIC_TOKEN: &str = include_str!("board_web/assets/state/board_web_token.js");
const PUBLIC_INGEST: &str = include_str!("board_web/assets/board_ingest.js");
const PUBLIC_LRU: &str = include_str!("board_web/assets/state/board_lru.js");
const PUBLIC_SEEN: &str = include_str!("board_web/assets/state/board_seen.js");

pub(crate) struct WebStore {
    config: Mutex<BoardConfigCache>,
    writer: Mutex<Option<LocalBoard>>,
    projects: Mutex<ProjectResolver>,
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
            projects: Mutex::new(ProjectResolver::default()),
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

    pub(crate) fn projects(
        &self,
        host: &str,
        expires: Instant,
    ) -> Result<Vec<ResolvedProject>, BoardError> {
        let config = self.config(expires)?;
        let mut projects = lock_until(&self.projects, expires)?;
        let deadline = crate::daemon::deadline::QueryDeadline::after(
            expires.saturating_duration_since(Instant::now()),
        );
        projects.resolve(&config, host, deadline)
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

pub(crate) struct BoardWebState {
    store: Arc<WebStore>,
    guard: WebGuard,
    streams: EventStreams,
    ingest: web_ops::IngestFlight,
    /// Database identity served in the shell; read once at bind because the
    /// server never changes databases without a restart.
    board_id: String,
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
