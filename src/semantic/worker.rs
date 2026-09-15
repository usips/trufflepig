//! Shared per-user inference worker and foreground client.

mod admission;
mod client;
mod engine;
mod lease;
mod load_retry;
mod protocol;
mod scheduler;

pub use admission::{AdmissionController, AdmissionLimits, AdmissionTicket};
pub use protocol::{
    Handshake, PROTOCOL_VERSION, read_frame, read_frame_with_deadline, write_frame,
};
pub use scheduler::{FairScheduler, RequestClass, RootIdent, ScheduledRequest};

use super::Embedding;
use crate::{background_process::spawn_background, semantic::runtime_config::InferenceConfig};
use anyhow::{Context, Result, bail, ensure};
use client::{configure_io, connect};
use engine::{Engine, open_engine};
use lease::{WorkerLease, WorkerSocket, worker_cache_dir};
use load_retry::LoadRetry;
use protocol::{WorkerCommand, WorkerReply, WorkerRequest, write_reply};
use std::{
    collections::HashSet,
    env,
    io::ErrorKind,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const QUERY_DEADLINE: Duration = Duration::from_millis(500);
const BACKGROUND_DEADLINE: Duration = Duration::from_secs(60);
const CONTROL_DEADLINE: Duration = Duration::from_millis(500);
const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);
const ACCEPT_SLEEP: Duration = Duration::from_millis(5);
const FRAME_READ_TIMEOUT: Duration = Duration::from_millis(25);
const FRAME_READ_DEADLINE: Duration = Duration::from_millis(500);
const WORKER_COMMAND: &str = "semantic-worker-serve";
const MAX_FRAME_READERS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbedKind {
    Query,
    Background,
}

/// Owns one foreground model and the same device/model lease as the worker.
/// Keep this value alive while a no-daemon indexing sweep submits batches.
pub struct ForegroundSession {
    engine: Box<dyn Engine>,
    _lease: WorkerLease,
}

impl ForegroundSession {
    pub fn open(root_id: &Path) -> Result<Self> {
        #[cfg(not(feature = "semantic"))]
        {
            let _ = root_id;
            return Err(crate::semantic::unavailable());
        }
        let config = InferenceConfig::load()?;
        require_foreground_cuda_mask(&config)?;
        let cache = worker_cache_dir()?;
        let lease = WorkerLease::acquire(&cache)?;
        let engine = open_engine(&config, &cache)?;
        let _ = root_id;
        Ok(Self {
            _lease: lease,
            engine,
        })
    }

    pub fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Embedding>> {
        protocol::validate_inputs(texts)?;
        self.engine.embed_batch(texts)
    }
}

impl EmbedKind {
    fn class(self) -> RequestClass {
        match self {
            Self::Query => RequestClass::Query,
            Self::Background => RequestClass::Background,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WorkerState {
    NotRunning,
    Loading,
    Ready,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WorkerStatus {
    pub state: WorkerState,
    pub provider: String,
    pub gpu_uuid: Option<String>,
    pub pending: usize,
    pub loaded: bool,
    pub config_fingerprint: String,
    pub model_fingerprint: String,
    pub handshake_match: bool,
    pub error: Option<String>,
}

impl WorkerStatus {
    fn not_running(config: &InferenceConfig) -> Self {
        let handshake = Handshake::for_config(config);
        Self {
            state: WorkerState::NotRunning,
            provider: config.provider.to_string(),
            gpu_uuid: config.gpu_uuid.clone(),
            pending: 0,
            loaded: false,
            config_fingerprint: handshake.config,
            model_fingerprint: handshake.model,
            handshake_match: true,
            error: None,
        }
    }

    fn loading(config: &InferenceConfig) -> Self {
        let mut status = Self::not_running(config);
        status.state = WorkerState::Loading;
        status
    }
}

/// Start the worker without waiting for model initialization.
pub fn ensure_started(cache_identity: &Path) -> Result<WorkerStatus> {
    #[cfg(not(feature = "semantic"))]
    {
        let _ = cache_identity;
        return Err(crate::semantic::unavailable());
    }
    let config = InferenceConfig::load()?;
    let cache = worker_cache_dir()?;
    let deadline = Instant::now() + CONTROL_DEADLINE;
    if let Ok(status) = request_status_until(&cache, &config, deadline) {
        return Ok(status);
    }
    start_worker(cache_identity, &config, &cache)?;
    Ok(WorkerStatus::loading(&config))
}

fn start_worker(cache_identity: &Path, config: &InferenceConfig, cache: &Path) -> Result<()> {
    let executable = env::var_os("TRUFFLEPIG_WORKER_BINARY")
        .map(PathBuf::from)
        .unwrap_or(env::current_exe()?);
    let mut command = Command::new(executable);
    command.args(["--no-daemon", WORKER_COMMAND]);
    command.env("TRUFFLEPIG_WORKER_CACHE", &cache);
    command.env("TRUFFLEPIG_WORKER_ROOT", cache_identity);
    if config.provider == crate::semantic::runtime_config::InferenceProvider::Cuda {
        if let Some(uuid) = config.gpu_uuid.as_deref() {
            // A UUID visibility mask makes CUDA ordinal zero refer to the
            // configured physical device regardless of host enumeration order.
            command.env("CUDA_VISIBLE_DEVICES", uuid);
        }
    }
    if let Some(runtime) = config.runtime_library.as_deref() {
        command.env("ORT_DYLIB_PATH", runtime);
    }
    let mut child = spawn_background(&mut command).context("start semantic inference worker")?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Return worker state without opening its model.
pub fn status(_: &Path) -> Result<WorkerStatus> {
    #[cfg(not(feature = "semantic"))]
    {
        return Err(crate::semantic::unavailable());
    }
    let config = InferenceConfig::load()?;
    let cache = worker_cache_dir()?;
    match request_status(&cache, &config) {
        Ok(status) => Ok(status),
        Err(error) if is_not_running(&error) => Ok(WorkerStatus::not_running(&config)),
        Err(error) => Err(error),
    }
}

/// Ask the worker to exit. This operation never initializes inference.
pub fn stop(_: &Path) -> Result<WorkerStatus> {
    #[cfg(not(feature = "semantic"))]
    {
        return Err(crate::semantic::unavailable());
    }
    let config = InferenceConfig::load()?;
    let cache = worker_cache_dir()?;
    let deadline = Instant::now() + CONTROL_DEADLINE;
    let stream = match connect(&cache, deadline) {
        Ok(stream) => stream,
        Err(error)
            if error.kind() == ErrorKind::NotFound
                || error.kind() == ErrorKind::ConnectionRefused =>
        {
            return Ok(WorkerStatus::not_running(&config));
        }
        Err(error) => return Err(error.into()),
    };
    let mut stream = stream;
    configure_io(&stream, deadline)?;
    let request = WorkerRequest::new(&config, WorkerCommand::Stop);
    protocol::write_request_with_deadline(&mut stream, &request, deadline)?;
    match protocol::read_reply_with_deadline(&mut stream, deadline)? {
        WorkerReply::Status(status) => Ok(status),
        WorkerReply::Error { message } => bail!("semantic_worker: {message}"),
        WorkerReply::Embeddings { .. } => bail!("semantic_worker: invalid stop response"),
    }
}

/// Embed one query through the shared worker, bounded end to end to 500 ms.
pub fn embed_query(cache_identity: &Path, text: &str) -> Result<Embedding> {
    let results = embed(EmbedKind::Query, cache_identity, &[text.to_owned()])?;
    results
        .into_iter()
        .next()
        .context("semantic_worker: missing query embedding")
}

/// Submit a bounded batch to the shared worker.
pub fn embed(kind: EmbedKind, root_id: &Path, texts: &[String]) -> Result<Vec<Embedding>> {
    #[cfg(not(feature = "semantic"))]
    {
        let _ = (kind, root_id, texts);
        return Err(crate::semantic::unavailable());
    }
    let started = Instant::now();
    let budget = match kind {
        EmbedKind::Query => QUERY_DEADLINE,
        EmbedKind::Background => BACKGROUND_DEADLINE,
    };
    let deadline_instant = started + budget;
    protocol::validate_inputs(texts)?;
    let config = InferenceConfig::load()?;
    let cache = worker_cache_dir()?;
    let remaining = deadline_instant.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("semantic_timeout: request exceeded its deadline before send");
    }
    let deadline_ms = SystemTime::now()
        .checked_add(remaining)
        .context("semantic_worker: system clock overflow")?
        .duration_since(UNIX_EPOCH)?
        .as_millis() as u64;
    let request = WorkerRequest::new(
        &config,
        WorkerCommand::Embed {
            class: kind.class(),
            root_id: root_id.to_string_lossy().into_owned(),
            texts: texts.to_vec(),
            deadline_ms,
        },
    );
    let mut stream = match connect(&cache, deadline_instant) {
        Ok(stream) => stream,
        Err(error)
            if error.kind() == ErrorKind::NotFound
                || error.kind() == ErrorKind::ConnectionRefused =>
        {
            let _ = ensure_started_until(root_id, deadline_instant)?;
            bail!("semantic_loading: inference worker is starting")
        }
        Err(error) => return Err(error.into()),
    };
    configure_io(&stream, deadline_instant)?;
    protocol::write_request_with_deadline(&mut stream, &request, deadline_instant)?;
    match protocol::read_reply_with_deadline(&mut stream, deadline_instant)? {
        WorkerReply::Embeddings { values } => values
            .into_iter()
            .map(|values| Embedding::from_values(&values))
            .collect(),
        WorkerReply::Error { message } => bail!("semantic_worker: {message}"),
        WorkerReply::Status(_) => bail!("semantic_worker: invalid embedding response"),
    }
}

/// Foreground embedding for `--no-daemon`; it acquires the worker's same lease.
pub fn embed_foreground(root_id: &Path, texts: &[String]) -> Result<Vec<Embedding>> {
    let mut session = ForegroundSession::open(root_id)?;
    session.embed_batch(texts)
}

pub fn embed_foreground_query(root_id: &Path, text: &str) -> Result<Embedding> {
    embed_foreground(root_id, &[text.to_owned()])?
        .into_iter()
        .next()
        .context("semantic_worker: missing embedding")
}

/// Run the worker command in the foreground. Model loading begins on first batch.
pub fn serve(cache_override: Option<&Path>) -> Result<()> {
    #[cfg(not(feature = "semantic"))]
    {
        let _ = cache_override;
        return Err(crate::semantic::unavailable());
    }
    let config = InferenceConfig::load()?;
    prepare_runtime(&config)?;
    let cache = match cache_override {
        Some(path) => path.to_owned(),
        None => env::var_os("TRUFFLEPIG_WORKER_CACHE")
            .map(PathBuf::from)
            .unwrap_or(worker_cache_dir()?),
    };
    let socket = WorkerSocket::bind(&cache)?;
    let handshake = Handshake::for_config(&config);
    let mut state = WorkerRuntime::new(config, cache, handshake);
    state.run(&socket)
}

fn prepare_runtime(config: &InferenceConfig) -> Result<()> {
    if let Some(runtime) = config.runtime_library.as_deref() {
        // Called before a worker loader thread is created. The runtime reads this
        // value when SemanticInferenceEngine initializes ONNX Runtime.
        unsafe {
            env::set_var("ORT_DYLIB_PATH", runtime);
        }
    }
    Ok(())
}

fn require_foreground_cuda_mask(config: &InferenceConfig) -> Result<()> {
    if config.provider != crate::semantic::runtime_config::InferenceProvider::Cuda {
        return Ok(());
    }
    let expected = config
        .gpu_uuid
        .as_deref()
        .context("inference_config_invalid: cuda requires gpu_uuid")?;
    let visible = env::var("CUDA_VISIBLE_DEVICES").unwrap_or_default();
    ensure!(
        visible == expected,
        "semantic_cuda: foreground inference requires CUDA_VISIBLE_DEVICES={expected}"
    );
    Ok(())
}

fn request_status(cache: &Path, config: &InferenceConfig) -> Result<WorkerStatus> {
    request_status_until(cache, config, Instant::now() + CONTROL_DEADLINE)
}

fn request_status_until(
    cache: &Path,
    config: &InferenceConfig,
    deadline: Instant,
) -> Result<WorkerStatus> {
    client::request_status(cache, config, deadline)
}

fn ensure_started_until(cache_identity: &Path, deadline: Instant) -> Result<WorkerStatus> {
    let config = InferenceConfig::load()?;
    let cache = worker_cache_dir()?;
    if let Ok(status) = request_status_until(&cache, &config, deadline) {
        return Ok(status);
    }
    ensure!(
        Instant::now() < deadline,
        "semantic_timeout: request exceeded its deadline while starting worker"
    );
    start_worker(cache_identity, &config, &cache)?;
    Ok(WorkerStatus::loading(&config))
}

fn is_not_running(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            )
        })
    })
}

struct QueuedConnection {
    request: protocol::WorkerRequest,
    stream: UnixStream,
    ticket: AdmissionTicket,
}

impl scheduler::RootIdent for QueuedConnection {
    fn root_id(&self) -> &str {
        match &self.request.command {
            WorkerCommand::Embed { root_id, .. } => root_id,
            WorkerCommand::Status | WorkerCommand::Stop => "",
        }
    }
}

struct WorkerRuntime {
    config: InferenceConfig,
    cache: PathBuf,
    handshake: Handshake,
    admission: AdmissionController,
    scheduler: FairScheduler<QueuedConnection>,
    background_roots: HashSet<String>,
    last_used: Option<Instant>,
    loading: bool,
    engine_loaded: bool,
    load_error: Option<String>,
    load_retry: LoadRetry,
    active: bool,
    executor_send: Sender<ExecutorCommand>,
    executor_receive: Receiver<ExecutorEvent>,
    stopping: bool,
}

struct RunningConnection {
    request: protocol::WorkerRequest,
    stream: UnixStream,
    inflight: admission::InflightTicket,
}

enum ExecutorCommand {
    Load {
        config: InferenceConfig,
        cache: PathBuf,
    },
    Execute(RunningConnection),
    Unload,
    Shutdown,
}

enum ExecutorEvent {
    Loaded(Result<(), String>),
    Completed {
        request: protocol::WorkerRequest,
        stream: UnixStream,
        inflight: admission::InflightTicket,
        result: Result<Vec<Embedding>, String>,
    },
    Unloaded,
}

impl WorkerRuntime {
    fn new(config: InferenceConfig, cache: PathBuf, handshake: Handshake) -> Self {
        let (executor_send, executor_commands) = mpsc::channel();
        let (executor_events, executor_receive) = mpsc::channel();
        thread::Builder::new()
            .name("semantic-inference".into())
            .spawn(move || inference_loop(executor_commands, executor_events))
            .expect("start semantic inference thread");
        Self {
            config,
            cache,
            handshake,
            admission: AdmissionController::default(),
            scheduler: FairScheduler::default(),
            background_roots: HashSet::new(),
            last_used: None,
            loading: false,
            engine_loaded: false,
            load_error: None,
            load_retry: LoadRetry::default(),
            active: false,
            executor_send,
            executor_receive,
            stopping: false,
        }
    }

    fn run(&mut self, socket: &WorkerSocket) -> Result<()> {
        let (parsed_send, parsed_receive) = mpsc::channel();
        let frame_readers = Arc::new(AtomicUsize::new(0));
        while !self.stopping {
            let mut did_work = false;
            while let Ok((stream, request)) = parsed_receive.try_recv() {
                did_work = true;
                self.handle_request(stream, request);
            }
            match socket.accept() {
                Ok((stream, _)) => {
                    did_work = true;
                    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
                    stream.set_read_timeout(Some(FRAME_READ_TIMEOUT))?;
                    if frame_readers
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                            (count < MAX_FRAME_READERS).then_some(count + 1)
                        })
                        .is_err()
                    {
                        let mut stream = stream;
                        let _ = write_reply(
                            &mut stream,
                            &WorkerReply::Error {
                                message: "semantic_admission: too many frame readers".into(),
                            },
                        );
                    } else {
                        let parsed_send = parsed_send.clone();
                        let frame_readers = Arc::clone(&frame_readers);
                        thread::spawn(move || {
                            let mut stream = stream;
                            let request = protocol::read_request_with_deadline(
                                &mut stream,
                                Instant::now() + FRAME_READ_DEADLINE,
                            )
                            .map_err(|error| format!("protocol: {error:#}"));
                            let _ = parsed_send.send((stream, request));
                            frame_readers.fetch_sub(1, Ordering::AcqRel);
                        });
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("accept semantic worker request"),
            }
            self.poll_executor();
            if !self.stopping && !self.active && (!self.loading || self.engine_loaded) {
                if let Some(item) = self.scheduler.pop() {
                    did_work = true;
                    self.process(item);
                }
            }
            self.unload_idle();
            if !did_work {
                thread::sleep(ACCEPT_SLEEP);
            }
        }
        let _ = self.executor_send.send(ExecutorCommand::Shutdown);
        Ok(())
    }

    fn handle_request(&mut self, mut stream: UnixStream, request: Result<WorkerRequest, String>) {
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                let _ = write_reply(
                    &mut stream,
                    &WorkerReply::Error {
                        message: format!("protocol: {error:#}"),
                    },
                );
                return;
            }
        };
        let handshake_match = request.handshake == self.handshake;
        match request.command {
            WorkerCommand::Status => {
                let _ = write_reply(
                    &mut stream,
                    &WorkerReply::Status(self.status(handshake_match)),
                );
            }
            WorkerCommand::Stop => {
                // The socket and private cache authenticate this control operation;
                // accepting it across config changes prevents orphaning a lease.
                let _ = write_reply(
                    &mut stream,
                    &WorkerReply::Status(WorkerStatus::not_running(&self.config)),
                );
                self.stopping = true;
            }
            WorkerCommand::Embed {
                class,
                root_id,
                texts,
                deadline_ms,
            } => {
                if !handshake_match {
                    let _ = write_reply(
                        &mut stream,
                        &WorkerReply::Error {
                            message:
                                "handshake_mismatch: protocol, build, config, or model changed"
                                    .into(),
                        },
                    );
                    return;
                }
                if class == RequestClass::Background && self.background_roots.contains(&root_id) {
                    let _ = write_reply(
                        &mut stream,
                        &WorkerReply::Error {
                            message: "semantic_root_busy: one outstanding batch per root".into(),
                        },
                    );
                    return;
                }
                let bytes = protocol::raw_input_bytes(&texts);
                let ticket = match self.admission.try_admit(texts.len(), bytes) {
                    Ok(ticket) => ticket,
                    Err(error) => {
                        let _ = write_reply(
                            &mut stream,
                            &WorkerReply::Error {
                                message: error.to_string(),
                            },
                        );
                        return;
                    }
                };
                if class == RequestClass::Background {
                    self.background_roots.insert(root_id.clone());
                }
                let queued = QueuedConnection {
                    request: WorkerRequest {
                        handshake: self.handshake.clone(),
                        command: WorkerCommand::Embed {
                            class,
                            root_id,
                            texts,
                            deadline_ms,
                        },
                    },
                    stream,
                    ticket,
                };
                self.scheduler.push(class, queued);
            }
        }
    }

    fn process(&mut self, item: QueuedConnection) {
        self.process_at(item, Instant::now());
    }

    fn process_at(&mut self, mut item: QueuedConnection, now: Instant) {
        let command = match &item.request.command {
            WorkerCommand::Embed {
                class,
                root_id,
                deadline_ms,
                ..
            } => (root_id.clone(), *deadline_ms, *class),
            _ => return,
        };
        let (root_id, deadline_ms, class) = command;
        if now_ms() >= deadline_ms {
            if let WorkerCommand::Embed {
                class: RequestClass::Background,
                ..
            } = item.request.command
            {
                self.background_roots.remove(&root_id);
            }
            return;
        }
        if let Some(error) = &self.load_error
            && !self.load_retry.is_ready(now)
        {
            let _ = write_reply(
                &mut item.stream,
                &WorkerReply::Error {
                    message: error.clone(),
                },
            );
            if let WorkerCommand::Embed {
                class: RequestClass::Background,
                ..
            } = item.request.command
            {
                self.background_roots.remove(&root_id);
            }
            return;
        }
        if !self.engine_loaded {
            if !self.loading {
                self.start_loader(now);
            }
            self.scheduler.push(class, item);
            return;
        }
        let inflight = item.ticket.begin();
        let connection = RunningConnection {
            request: item.request,
            stream: item.stream,
            inflight,
        };
        self.active = true;
        if self
            .executor_send
            .send(ExecutorCommand::Execute(connection))
            .is_err()
        {
            self.active = false;
            if class == RequestClass::Background {
                self.background_roots.remove(&root_id);
            }
        }
    }

    fn unload_idle(&mut self) {
        if self.scheduler.pending() != 0 || self.loading || self.active || !self.engine_loaded {
            return;
        }
        if self
            .last_used
            .is_some_and(|used| used.elapsed() >= IDLE_UNLOAD)
        {
            let _ = self.executor_send.send(ExecutorCommand::Unload);
            self.engine_loaded = false;
            self.last_used = None;
        }
    }

    fn start_loader(&mut self, now: Instant) {
        self.loading = true;
        if self
            .executor_send
            .send(ExecutorCommand::Load {
                config: self.config.clone(),
                cache: self.cache.clone(),
            })
            .is_err()
        {
            self.record_load_failure("semantic_worker: model loader exited".into(), now);
        }
    }

    fn record_load_failure(&mut self, error: String, now: Instant) {
        self.loading = false;
        self.engine_loaded = false;
        self.load_error = Some(error);
        self.load_retry.failed(now);
    }

    fn poll_executor(&mut self) {
        loop {
            match self.executor_receive.try_recv() {
                Ok(ExecutorEvent::Loaded(Ok(()))) => {
                    self.loading = false;
                    self.engine_loaded = true;
                    self.load_retry.succeeded();
                    self.load_error = None;
                    self.last_used = Some(Instant::now());
                }
                Ok(ExecutorEvent::Loaded(Err(error))) => {
                    self.record_load_failure(error, Instant::now());
                }
                Ok(ExecutorEvent::Completed {
                    request,
                    mut stream,
                    inflight,
                    result,
                }) => {
                    self.active = false;
                    self.last_used = Some(Instant::now());
                    let (root_id, deadline_ms, class) = match request.command {
                        WorkerCommand::Embed {
                            root_id,
                            deadline_ms,
                            class,
                            ..
                        } => (root_id, deadline_ms, class),
                        WorkerCommand::Status | WorkerCommand::Stop => {
                            (String::new(), 0, RequestClass::Query)
                        }
                    };
                    if class == RequestClass::Background {
                        self.background_roots.remove(&root_id);
                    }
                    if now_ms() < deadline_ms {
                        match result {
                            Ok(values) => {
                                let values =
                                    values.into_iter().map(|value| value.0.to_vec()).collect();
                                let _ =
                                    write_reply(&mut stream, &WorkerReply::Embeddings { values });
                            }
                            Err(error) => {
                                let _ = write_reply(
                                    &mut stream,
                                    &WorkerReply::Error { message: error },
                                );
                            }
                        }
                    }
                    drop(inflight);
                }
                Ok(ExecutorEvent::Unloaded) => {
                    self.engine_loaded = false;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn status(&self, handshake_match: bool) -> WorkerStatus {
        let state = if self.loading {
            WorkerState::Loading
        } else if self.engine_loaded {
            WorkerState::Ready
        } else {
            WorkerState::Loading
        };
        WorkerStatus {
            state,
            provider: self.config.provider.to_string(),
            gpu_uuid: self.config.gpu_uuid.clone(),
            pending: self.scheduler.pending() + usize::from(self.active),
            loaded: self.engine_loaded,
            config_fingerprint: self.handshake.config.clone(),
            model_fingerprint: self.handshake.model.clone(),
            handshake_match,
            error: self.load_error.clone(),
        }
    }
}

fn inference_loop(commands: Receiver<ExecutorCommand>, events: Sender<ExecutorEvent>) {
    let mut engine: Option<Box<dyn Engine>> = None;
    for command in commands {
        match command {
            ExecutorCommand::Load { config, cache } => match open_engine(&config, &cache) {
                Ok(opened) => {
                    engine = Some(opened);
                    let _ = events.send(ExecutorEvent::Loaded(Ok(())));
                }
                Err(error) => {
                    let _ = events.send(ExecutorEvent::Loaded(Err(format!("{error:#}"))));
                }
            },
            ExecutorCommand::Execute(connection) => {
                let RunningConnection {
                    request,
                    stream,
                    inflight,
                } = connection;
                let result = match (&mut engine, &request.command) {
                    (Some(engine), WorkerCommand::Embed { texts, .. }) => engine
                        .embed_batch(texts)
                        .map_err(|error| format!("{error:#}")),
                    (None, _) => Err("semantic_worker: engine unavailable".into()),
                    (Some(_), _) => Err("semantic_worker: invalid inference command".into()),
                };
                let _ = events.send(ExecutorEvent::Completed {
                    request,
                    stream,
                    inflight,
                    result,
                });
            }
            ExecutorCommand::Unload => {
                engine = None;
                let _ = events.send(ExecutorEvent::Unloaded);
            }
            ExecutorCommand::Shutdown => break,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn queued_query(config: &InferenceConfig) -> (QueuedConnection, UnixStream) {
        let (stream, peer) = UnixStream::pair().unwrap();
        let request = WorkerRequest::new(
            config,
            WorkerCommand::Embed {
                class: RequestClass::Query,
                root_id: "test-root".into(),
                texts: vec!["query".into()],
                deadline_ms: now_ms() + 10_000,
            },
        );
        let ticket = AdmissionController::default().try_admit(1, 5).unwrap();
        (
            QueuedConnection {
                request,
                stream,
                ticket,
            },
            peer,
        )
    }

    #[test]
    fn loaded_model_unloads_without_a_completed_query() {
        let config = InferenceConfig::default();
        let handshake = Handshake::for_config(&config);
        let mut runtime = WorkerRuntime::new(config, PathBuf::new(), handshake);
        let (events, receive) = mpsc::channel();
        let (send, commands) = mpsc::channel();
        runtime.executor_receive = receive;
        runtime.executor_send = send;
        runtime.loading = true;
        events.send(ExecutorEvent::Loaded(Ok(()))).unwrap();
        runtime.poll_executor();
        assert!(runtime.last_used.is_some());
        runtime.last_used = Some(Instant::now() - IDLE_UNLOAD);
        runtime.unload_idle();
        assert!(!runtime.engine_loaded);
        assert!(matches!(commands.try_recv(), Ok(ExecutorCommand::Unload)));
    }

    #[test]
    fn failed_gpu_load_retries_after_cooldown_without_a_retry_storm() {
        let config = InferenceConfig::default();
        let handshake = Handshake::for_config(&config);
        let mut runtime = WorkerRuntime::new(config.clone(), PathBuf::new(), handshake);
        let (events, receive) = mpsc::channel();
        let (send, commands) = mpsc::channel();
        runtime.executor_receive = receive;
        runtime.executor_send = send;
        runtime.loading = true;

        events
            .send(ExecutorEvent::Loaded(
                Err("cuda_unavailable: no GPU".into()),
            ))
            .unwrap();
        runtime.poll_executor();
        let status = runtime.status(true);
        assert_eq!(status.error.as_deref(), Some("cuda_unavailable: no GPU"));
        assert!(!status.loaded);
        let retry_at = runtime.load_retry.retry_at().unwrap();

        let (item, _peer) = queued_query(&config);
        runtime.process_at(item, retry_at - Duration::from_nanos(1));
        assert!(commands.try_recv().is_err());

        let (item, _peer) = queued_query(&config);
        runtime.process_at(item, retry_at);
        assert!(runtime.loading);
        assert!(matches!(
            commands.try_recv(),
            Ok(ExecutorCommand::Load { .. })
        ));
        assert!(commands.try_recv().is_err());

        let (item, _peer) = queued_query(&config);
        runtime.process_at(item, retry_at);
        assert!(commands.try_recv().is_err());

        events.send(ExecutorEvent::Loaded(Ok(()))).unwrap();
        runtime.poll_executor();
        let status = runtime.status(true);
        assert!(status.loaded);
        assert!(status.error.is_none());
    }

    #[test]
    fn worker_constants_preserve_query_budget() {
        assert_eq!(QUERY_DEADLINE, Duration::from_millis(500));
        assert_eq!(IDLE_UNLOAD, Duration::from_secs(600));
    }
}
