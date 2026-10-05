//! Listener bind and the bounded connection-accept loop.
use super::{
    BoardWebState, WebStore, bootstrap_line, http_wire, open_stream_feed,
    serve_lock::{self, ServeLock},
    signal_shutdown,
    web_endpoint::{self, EndpointGuard},
    web_guard::WebGuard,
    web_ops, web_routes,
};
use super::super::board_config::BoardConfigCache;
use super::event_stream::SequencePoller;
use crate::daemon::{PoolSize, RequestPool};
use anyhow::{Context, Result};
use std::{
    io::{self, IsTerminal, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::Arc,
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
    endpoint: EndpointGuard,
    _serve_lock: ServeLock,
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
        let board_id = store.with_writer(
            &config,
            Instant::now() + http_wire::REQUEST_TIMEOUT,
            |writer| writer.board_uuid(),
        )?;
        web_endpoint::publish(&store.runtime, listener.local_addr()?, &config.db_path)?;
        let endpoint = EndpointGuard::arm(&store.runtime, listener.local_addr()?);
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
            endpoint,
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

/// Refuses a connection the pool cannot take: one nonblocking busy write,
/// then a short-lived thread drains unread input after shutting down the
/// write side, so the client reads the reply instead of a reset. The
/// accept loop never blocks on a refused connection.
pub(super) fn refuse_queue_full(mut refusal: TcpStream, busy: &[u8]) {
    // One nonblocking attempt never holds up accepting the next connection.
    if refusal.set_nonblocking(true).is_ok() {
        let _ = refusal.write(busy);
    }
    // A failed spawn drops the socket, as an immediate close would.
    let _ = std::thread::Builder::new()
        .name("board-refusal-drain".into())
        .spawn(move || {
            let _ = refusal.set_nonblocking(false);
            http_wire::close_after_error(&mut refusal);
        });
}

pub(super) fn serve_connections(
    incoming: impl IntoIterator<Item = io::Result<TcpStream>>,
    state: &Arc<BoardWebState>,
) -> Result<()> {
    let pool = RequestPool::new("board-http", WEB_POOL)?;
    let busy = http_wire::unavailable_response(1, QUEUE_FULL_BODY);
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
            if let Some(refusal) = refusal {
                refuse_queue_full(refusal, &busy);
            }
        }
    }
    Ok(())
}
