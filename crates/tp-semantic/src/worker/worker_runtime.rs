//! The worker process's socket runtime: admission, fair scheduling, model
//! residency, and the inference executor thread.

use super::admission::{self, AdmissionController, AdmissionTicket};
use super::engine::{Engine, open_engine};
use super::lease::WorkerSocket;
use super::load_retry::LoadRetry;
use super::protocol::{self, Handshake, WorkerCommand, WorkerReply, WorkerRequest, write_reply};
use super::rerank_executor::RerankLane;
use super::scheduler::{self, FairScheduler, RequestClass};
use super::status::{WorkerState, WorkerStatus};
use crate::{Embedding, VerifiedAssetDigests, runtime_config::InferenceConfig};
use anyhow::{Context, Result};
use std::{
    collections::HashSet,
    io::ErrorKind,
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const ACCEPT_SLEEP: Duration = Duration::from_millis(5);
const FRAME_READ_TIMEOUT: Duration = Duration::from_millis(25);
const FRAME_READ_DEADLINE: Duration = Duration::from_millis(500);
const MAX_FRAME_READERS: usize = 64;

pub(super) struct QueuedConnection {
    pub(super) request: protocol::WorkerRequest,
    pub(super) stream: UnixStream,
    pub(super) ticket: AdmissionTicket,
}

impl scheduler::RootIdent for QueuedConnection {
    fn root_id(&self) -> &str {
        match &self.request.command {
            WorkerCommand::Embed { root_id, .. } => root_id,
            WorkerCommand::Rerank { root_id, .. } => root_id,
            WorkerCommand::Status | WorkerCommand::Stop => "",
        }
    }
}

pub(super) struct WorkerRuntime {
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
    rerank_lane: RerankLane,
    stopping: bool,
}

pub(super) struct RunningConnection {
    pub(super) request: protocol::WorkerRequest,
    pub(super) stream: UnixStream,
    pub(super) inflight: admission::InflightTicket,
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
    pub(super) fn new(config: InferenceConfig, cache: PathBuf, handshake: Handshake) -> Self {
        let (executor_send, executor_commands) = mpsc::channel();
        let (executor_events, executor_receive) = mpsc::channel();
        thread::Builder::new()
            .name("semantic-inference".into())
            .spawn(move || inference_loop(executor_commands, executor_events))
            .expect("start semantic inference thread");
        let rerank_lane = RerankLane::new(config.clone());
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
            rerank_lane,
            stopping: false,
        }
    }

    pub(super) fn run(&mut self, socket: &WorkerSocket) -> Result<()> {
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
            did_work = self.rerank_lane.pump(Instant::now()) || did_work;
            if !self.stopping && !self.active && (!self.loading || self.engine_loaded) {
                if let Some(item) = self.scheduler.pop() {
                    did_work = true;
                    self.process(item);
                }
            }
            if !self.stopping {
                did_work = self.rerank_lane.poll() || did_work;
            }
            self.unload_idle();
            self.rerank_lane.unload_idle();
            if !did_work {
                thread::sleep(ACCEPT_SLEEP);
            }
        }
        let _ = self.executor_send.send(ExecutorCommand::Shutdown);
        self.rerank_lane.shutdown();
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
            command @ WorkerCommand::Rerank { .. } => {
                self.rerank_lane.admit(
                    &self.handshake,
                    &self.admission,
                    handshake_match,
                    command,
                    stream,
                );
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

    /// Unloads the embedder after the configured idle period; without one it
    /// stays resident.
    fn unload_idle(&mut self) {
        let Some(idle_unload) = self.config.idle_unload() else {
            return;
        };
        if self.scheduler.pending() != 0 || self.loading || self.active || !self.engine_loaded {
            return;
        }
        if self
            .last_used
            .is_some_and(|used| used.elapsed() >= idle_unload)
        {
            let _ = self.executor_send.send(ExecutorCommand::Unload);
            self.engine_loaded = false;
            self.last_used = None;
        }
    }

    /// Starts loading a configured embedder on the executor thread so the first
    /// query need not pay for a cold start within its 500 ms deadline.
    pub(super) fn start_eager_load(&mut self, now: Instant) {
        if self.config.model_dir.is_some() && !self.loading && !self.engine_loaded {
            self.start_loader(now);
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
                        WorkerCommand::Status
                        | WorkerCommand::Stop
                        | WorkerCommand::Rerank { .. } => (String::new(), 0, RequestClass::Query),
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
        let rerank = self.rerank_lane.status_fields();
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
            rerank_loaded: rerank.loaded,
            rerank_gpu_uuid: rerank.gpu_uuid,
            rerank_error: rerank.error,
            rerank_pending: rerank.pending,
        }
    }
}

fn inference_loop(commands: Receiver<ExecutorCommand>, events: Sender<ExecutorEvent>) {
    let mut engine: Option<Box<dyn Engine>> = None;
    // Lives as long as this thread, so a reload skips re-hashing unchanged assets.
    let mut digests = VerifiedAssetDigests::default();
    for command in commands {
        match command {
            ExecutorCommand::Load { config, cache } => {
                match open_engine(&config, &cache, &mut digests) {
                    Ok(opened) => {
                        engine = Some(opened);
                        let _ = events.send(ExecutorEvent::Loaded(Ok(())));
                    }
                    Err(error) => {
                        let _ = events.send(ExecutorEvent::Loaded(Err(format!("{error:#}"))));
                    }
                }
            }
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

pub(super) fn now_ms() -> u64 {
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

    fn loaded_runtime(config: InferenceConfig) -> (WorkerRuntime, Receiver<ExecutorCommand>) {
        let handshake = Handshake::for_config(&config);
        let mut runtime = WorkerRuntime::new(config, PathBuf::new(), handshake);
        let (events, receive) = mpsc::channel();
        let (send, commands) = mpsc::channel();
        runtime.executor_receive = receive;
        runtime.executor_send = send;
        runtime.loading = true;
        events.send(ExecutorEvent::Loaded(Ok(()))).unwrap();
        runtime.poll_executor();
        (runtime, commands)
    }

    #[test]
    fn loaded_model_stays_resident_without_idle_unload() {
        let (mut runtime, commands) = loaded_runtime(InferenceConfig::default());
        runtime.last_used = Some(Instant::now() - Duration::from_secs(24 * 60 * 60));
        runtime.unload_idle();
        assert!(runtime.engine_loaded);
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn loaded_model_unloads_without_a_completed_query() {
        let (mut runtime, commands) = loaded_runtime(InferenceConfig {
            idle_unload_seconds: Some(600),
            ..InferenceConfig::default()
        });
        assert!(runtime.last_used.is_some());
        runtime.last_used = Some(Instant::now() - Duration::from_secs(600));
        runtime.unload_idle();
        assert!(!runtime.engine_loaded);
        assert!(matches!(commands.try_recv(), Ok(ExecutorCommand::Unload)));
    }

    #[test]
    fn startup_loads_a_configured_embedder_once_without_a_request() {
        let unconfigured = InferenceConfig::default();
        let handshake = Handshake::for_config(&unconfigured);
        let mut runtime = WorkerRuntime::new(unconfigured, PathBuf::new(), handshake);
        let (send, commands) = mpsc::channel();
        runtime.executor_send = send;
        runtime.start_eager_load(Instant::now());
        assert!(!runtime.loading);
        assert!(commands.try_recv().is_err());

        let config = InferenceConfig {
            model_dir: Some("/models/embedder".into()),
            ..InferenceConfig::default()
        };
        let handshake = Handshake::for_config(&config);
        let mut runtime = WorkerRuntime::new(config, PathBuf::new(), handshake);
        let (send, commands) = mpsc::channel();
        runtime.executor_send = send;
        runtime.start_eager_load(Instant::now());
        runtime.start_eager_load(Instant::now());
        assert!(runtime.loading);
        assert!(matches!(
            commands.try_recv(),
            Ok(ExecutorCommand::Load { .. })
        ));
        assert!(commands.try_recv().is_err());
        assert_eq!(runtime.status(true).state, WorkerState::Loading);
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
}
