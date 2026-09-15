//! The reranking lane: its own load/unload lifecycle and executor thread,
//! independent of the embedding engine so both can run on separate GPUs.

use super::engine::{RerankEngine, open_rerank_engine};
use super::load_retry::LoadRetry;
use super::protocol::{self, WorkerCommand, WorkerReply, write_reply};
use super::{QueuedConnection, RunningConnection, now_ms};
use crate::semantic::runtime_config::InferenceConfig;
use std::{
    collections::VecDeque,
    os::unix::net::UnixStream,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::Instant,
};

enum RerankExecutorCommand {
    Load(InferenceConfig),
    Execute(RunningConnection),
    Unload,
    Shutdown,
}

enum RerankExecutorEvent {
    Loaded(Result<(), String>),
    Completed {
        request: protocol::WorkerRequest,
        stream: UnixStream,
        inflight: super::admission::InflightTicket,
        result: Result<Vec<f32>, String>,
    },
    Unloaded,
}

/// Snapshot of rerank lane state for [`super::WorkerStatus`].
pub(super) struct RerankStatusFields {
    pub loaded: bool,
    pub gpu_uuid: Option<String>,
    pub error: Option<String>,
    pub pending: usize,
}

/// Owns the reranker's load state, its own FIFO queue, and its executor
/// thread. Mirrors the embedding executor in `worker.rs` but never blocks on
/// or competes with it: the two lanes load, run, and idle-unload separately.
pub(super) struct RerankLane {
    config: InferenceConfig,
    queue: VecDeque<QueuedConnection>,
    loading: bool,
    loaded: bool,
    load_error: Option<String>,
    load_retry: LoadRetry,
    active: bool,
    last_used: Option<Instant>,
    executor_send: Sender<RerankExecutorCommand>,
    executor_receive: Receiver<RerankExecutorEvent>,
}

impl RerankLane {
    /// Builds the lane and, when a reranker is configured, starts loading it
    /// immediately so the first `Rerank` request need not pay for a cold
    /// start it triggered itself.
    pub(super) fn new(config: InferenceConfig) -> Self {
        let (executor_send, executor_commands) = mpsc::channel();
        let (executor_events, executor_receive) = mpsc::channel();
        thread::Builder::new()
            .name("semantic-rerank".into())
            .spawn(move || rerank_inference_loop(executor_commands, executor_events))
            .expect("start semantic rerank thread");
        let rerank_enabled = config.rerank_enabled();
        let mut lane = Self {
            config,
            queue: VecDeque::new(),
            loading: false,
            loaded: false,
            load_error: None,
            load_retry: LoadRetry::default(),
            active: false,
            last_used: None,
            executor_send,
            executor_receive,
        };
        if rerank_enabled {
            lane.start_loader();
        }
        lane
    }

    /// Queues a `Rerank` connection for FIFO dispatch.
    pub(super) fn enqueue(&mut self, item: QueuedConnection) {
        self.queue.push_back(item);
    }

    /// Validates the handshake and admission bounds for one `Rerank`
    /// request, replying with an error directly on either failure. When the
    /// reranker is not loaded, this starts (or lets run) its loader and
    /// replies `rerank_loading` immediately rather than parking the
    /// connection past its deadline; only a request to an already-loaded
    /// reranker is queued. `admission` is the same controller the embedding
    /// lane uses. `command` must be [`WorkerCommand::Rerank`].
    pub(super) fn admit(
        &mut self,
        handshake: &protocol::Handshake,
        admission: &super::AdmissionController,
        handshake_match: bool,
        command: WorkerCommand,
        mut stream: UnixStream,
    ) {
        let WorkerCommand::Rerank {
            root_id,
            query,
            documents,
            deadline_ms,
        } = command
        else {
            return;
        };
        if !handshake_match {
            let _ = write_reply(
                &mut stream,
                &WorkerReply::Error {
                    message: "handshake_mismatch: protocol, build, config, or model changed".into(),
                },
            );
            return;
        }
        if now_ms() >= deadline_ms {
            // The deadline already elapsed before admission; drop silently so
            // the peer's read hits EOF instead of waiting on a reply that
            // would arrive too late to matter.
            return;
        }
        let bytes = query.len() + protocol::raw_input_bytes(&documents);
        let ticket = match admission.try_admit(1, bytes) {
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
        if !self.loaded {
            let cooldown_blocks =
                self.load_error.is_some() && !self.load_retry.is_ready(Instant::now());
            if !self.loading && !cooldown_blocks {
                self.start_loader();
            }
            let message = if cooldown_blocks {
                self.load_error.clone().unwrap_or_else(|| {
                    "rerank_loading: reranker is loading".into()
                })
            } else {
                "rerank_loading: reranker is loading".into()
            };
            let _ = write_reply(&mut stream, &WorkerReply::Error { message });
            return;
        }
        self.enqueue(QueuedConnection {
            request: protocol::WorkerRequest {
                handshake: handshake.clone(),
                command: WorkerCommand::Rerank {
                    root_id,
                    query,
                    documents,
                    deadline_ms,
                },
            },
            stream,
            ticket,
        });
    }

    fn pending(&self) -> usize {
        self.queue.len() + usize::from(self.active)
    }

    /// Dispatches the next queued connection when the lane is idle. Returns
    /// whether it dispatched one, so the run loop can fold it into its
    /// overall `did_work` signal.
    pub(super) fn poll(&mut self) -> bool {
        if self.active || (self.loading && !self.loaded) {
            return false;
        }
        let Some(item) = self.queue.pop_front() else {
            return false;
        };
        self.process_at(item, Instant::now());
        true
    }

    fn process_at(&mut self, mut item: QueuedConnection, now: Instant) {
        let deadline_ms = match &item.request.command {
            WorkerCommand::Rerank { deadline_ms, .. } => *deadline_ms,
            _ => return,
        };
        if now_ms() >= deadline_ms {
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
            return;
        }
        if !self.loaded {
            if !self.loading {
                self.start_loader();
            }
            self.queue.push_front(item);
            return;
        }
        let inflight = item.ticket.begin();
        let connection = RunningConnection {
            request: item.request,
            stream: item.stream,
            inflight,
        };
        self.active = true;
        if let Err(mpsc::SendError(RerankExecutorCommand::Execute(mut connection))) =
            self.executor_send.send(RerankExecutorCommand::Execute(connection))
        {
            self.active = false;
            self.record_load_failure(
                "rerank_unavailable: rerank executor exited".into(),
                Instant::now(),
            );
            let _ = write_reply(
                &mut connection.stream,
                &WorkerReply::Error {
                    message: "rerank_unavailable: rerank executor exited".into(),
                },
            );
        }
    }

    fn start_loader(&mut self) {
        self.loading = true;
        if self
            .executor_send
            .send(RerankExecutorCommand::Load(self.config.clone()))
            .is_err()
        {
            self.record_load_failure(
                "rerank_unavailable: model loader exited".into(),
                Instant::now(),
            );
        }
    }

    fn record_load_failure(&mut self, error: String, now: Instant) {
        self.loading = false;
        self.loaded = false;
        self.load_error = Some(error);
        self.load_retry.failed(now);
    }

    /// Drains completed executor events, replying within each request's
    /// deadline. Returns whether it drained at least one event, so the run
    /// loop can fold it into its overall `did_work` signal.
    pub(super) fn pump(&mut self, now: Instant) -> bool {
        let mut did_work = false;
        loop {
            match self.executor_receive.try_recv() {
                Ok(RerankExecutorEvent::Loaded(Ok(()))) => {
                    did_work = true;
                    self.loading = false;
                    self.loaded = true;
                    self.load_retry.succeeded();
                    self.load_error = None;
                    self.last_used = Some(now);
                }
                Ok(RerankExecutorEvent::Loaded(Err(error))) => {
                    did_work = true;
                    self.record_load_failure(error, now);
                }
                Ok(RerankExecutorEvent::Completed {
                    request,
                    mut stream,
                    inflight,
                    result,
                }) => {
                    did_work = true;
                    self.active = false;
                    self.last_used = Some(now);
                    let deadline_ms = match request.command {
                        WorkerCommand::Rerank { deadline_ms, .. } => deadline_ms,
                        _ => 0,
                    };
                    if now_ms() < deadline_ms {
                        match result {
                            Ok(values) => {
                                let _ = write_reply(&mut stream, &WorkerReply::Scores { values });
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
                Ok(RerankExecutorEvent::Unloaded) => {
                    did_work = true;
                    self.loaded = false;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    did_work = true;
                    self.active = false;
                    self.record_load_failure(
                        "rerank_unavailable: rerank executor exited".into(),
                        now,
                    );
                    self.drain_queue_with_error();
                    break;
                }
            }
        }
        did_work
    }

    /// Replies `WorkerReply::Error` to every queued connection and drops
    /// their tickets, used once the executor thread has exited and no
    /// queued request can ever be dispatched.
    fn drain_queue_with_error(&mut self) {
        let message = self
            .load_error
            .clone()
            .unwrap_or_else(|| "rerank_unavailable: rerank executor exited".into());
        while let Some(mut item) = self.queue.pop_front() {
            let _ = write_reply(&mut item.stream, &WorkerReply::Error { message: message.clone() });
        }
    }

    /// Unloads the reranker after it sits idle for [`super::IDLE_UNLOAD`],
    /// independent of the embedding engine's own idle unload.
    pub(super) fn unload_idle(&mut self) {
        if !self.queue.is_empty() || self.loading || self.active || !self.loaded {
            return;
        }
        if self
            .last_used
            .is_some_and(|used| used.elapsed() >= super::IDLE_UNLOAD)
        {
            let _ = self.executor_send.send(RerankExecutorCommand::Unload);
            self.loaded = false;
            self.last_used = None;
        }
    }

    pub(super) fn status_fields(&self) -> RerankStatusFields {
        RerankStatusFields {
            loaded: self.loaded,
            gpu_uuid: self
                .config
                .rerank_enabled()
                .then(|| {
                    self.config
                        .rerank_gpu_uuid
                        .clone()
                        .or_else(|| self.config.gpu_uuid.clone())
                })
                .flatten(),
            error: self.load_error.clone(),
            pending: self.pending(),
        }
    }

    pub(super) fn shutdown(&self) {
        let _ = self.executor_send.send(RerankExecutorCommand::Shutdown);
    }
}

fn rerank_inference_loop(
    commands: Receiver<RerankExecutorCommand>,
    events: Sender<RerankExecutorEvent>,
) {
    let mut engine: Option<Box<dyn RerankEngine>> = None;
    for command in commands {
        match command {
            RerankExecutorCommand::Load(config) => match open_rerank_engine(&config) {
                Ok(opened) => {
                    engine = Some(opened);
                    let _ = events.send(RerankExecutorEvent::Loaded(Ok(())));
                }
                Err(error) => {
                    let _ = events.send(RerankExecutorEvent::Loaded(Err(format!("{error:#}"))));
                }
            },
            RerankExecutorCommand::Execute(connection) => {
                let RunningConnection {
                    request,
                    stream,
                    inflight,
                } = connection;
                let result = match (&mut engine, &request.command) {
                    (
                        Some(engine),
                        WorkerCommand::Rerank {
                            query, documents, ..
                        },
                    ) => engine
                        .rerank(query, documents)
                        .map_err(|error| format!("{error:#}")),
                    (None, _) => Err("rerank_unavailable: engine unavailable".into()),
                    (Some(_), _) => Err("rerank_unavailable: invalid inference command".into()),
                };
                let _ = events.send(RerankExecutorEvent::Completed {
                    request,
                    stream,
                    inflight,
                    result,
                });
            }
            RerankExecutorCommand::Unload => {
                engine = None;
                let _ = events.send(RerankExecutorEvent::Unloaded);
            }
            RerankExecutorCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::runtime_config::InferenceConfig;
    use crate::semantic::worker::protocol::WorkerRequest;
    use crate::semantic::worker::{AdmissionController, AdmissionLimits};
    use std::io::Read;

    fn queued_rerank(config: &InferenceConfig, deadline_ms: u64) -> (QueuedConnection, UnixStream) {
        let (stream, peer) = UnixStream::pair().unwrap();
        let request = WorkerRequest::new(
            config,
            WorkerCommand::Rerank {
                root_id: "test-root".into(),
                query: "query".into(),
                documents: vec!["doc".into()],
                deadline_ms,
            },
        );
        let ticket = AdmissionController::default().try_admit(1, 8).unwrap();
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
    fn queued_rerank_triggers_exactly_one_load() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let (events, receive) = mpsc::channel();
        let (send, commands) = mpsc::channel();
        lane.executor_receive = receive;
        lane.executor_send = send;

        let (item, _peer) = queued_rerank(&config, now_ms() + 10_000);
        lane.enqueue(item);
        lane.poll();

        assert!(lane.loading);
        assert!(matches!(
            commands.try_recv(),
            Ok(RerankExecutorCommand::Load(_))
        ));
        assert!(commands.try_recv().is_err());
        drop(events);
    }

    #[test]
    fn rerank_load_failure_sets_error_without_marking_loaded() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let (events, receive) = mpsc::channel();
        let (send, _commands) = mpsc::channel();
        lane.executor_receive = receive;
        lane.executor_send = send;

        events
            .send(RerankExecutorEvent::Loaded(Err(
                "rerank_unavailable: no reranker".into(),
            )))
            .unwrap();
        lane.pump(Instant::now());

        let status = lane.status_fields();
        assert!(!status.loaded);
        assert_eq!(
            status.error.as_deref(),
            Some("rerank_unavailable: no reranker")
        );
    }

    #[test]
    fn completed_rerank_event_writes_scores_reply() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let (events, receive) = mpsc::channel();
        let (send, _commands) = mpsc::channel();
        lane.executor_receive = receive;
        lane.executor_send = send;

        let (item, mut peer) = queued_rerank(&config, now_ms() + 10_000);
        let inflight = AdmissionController::default()
            .try_admit(1, 8)
            .unwrap()
            .begin();
        events
            .send(RerankExecutorEvent::Completed {
                request: item.request,
                stream: item.stream,
                inflight,
                result: Ok(vec![0.5, 0.75]),
            })
            .unwrap();
        lane.pump(Instant::now());

        let deadline = Instant::now() + std::time::Duration::from_millis(500);
        match protocol::read_reply_with_deadline(&mut peer, deadline).unwrap() {
            WorkerReply::Scores { values } => assert_eq!(values, vec![0.5, 0.75]),
            other => panic!("expected Scores reply, got {other:?}"),
        }
    }

    fn rerank_command(deadline_ms: u64) -> WorkerCommand {
        WorkerCommand::Rerank {
            root_id: "test-root".into(),
            query: "query".into(),
            documents: vec!["doc".into()],
            deadline_ms,
        }
    }

    #[test]
    fn admit_on_handshake_mismatch_replies_with_error() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let handshake = protocol::Handshake::for_config(&config);
        let admission = AdmissionController::default();
        let (stream, mut peer) = UnixStream::pair().unwrap();

        lane.admit(
            &handshake,
            &admission,
            false,
            rerank_command(now_ms() + 10_000),
            stream,
        );

        let deadline = Instant::now() + std::time::Duration::from_millis(500);
        match protocol::read_reply_with_deadline(&mut peer, deadline).unwrap() {
            WorkerReply::Error { message } => assert!(message.contains("handshake_mismatch")),
            other => panic!("expected Error reply, got {other:?}"),
        }
    }

    #[test]
    fn admit_on_admission_rejection_replies_with_error() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let handshake = protocol::Handshake::for_config(&config);
        let admission = AdmissionController::new(AdmissionLimits {
            max_admitted: 1,
            max_raw_input_bytes: 4,
            max_inputs: 8,
        });
        let (stream, mut peer) = UnixStream::pair().unwrap();

        lane.admit(
            &handshake,
            &admission,
            true,
            rerank_command(now_ms() + 10_000),
            stream,
        );

        let deadline = Instant::now() + std::time::Duration::from_millis(500);
        match protocol::read_reply_with_deadline(&mut peer, deadline).unwrap() {
            WorkerReply::Error { message } => assert!(message.contains("semantic_admission")),
            other => panic!("expected Error reply, got {other:?}"),
        }
    }

    #[test]
    fn admit_while_unloaded_replies_loading_and_starts_exactly_one_load() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let (_events, receive) = mpsc::channel();
        let (send, commands) = mpsc::channel();
        lane.executor_receive = receive;
        lane.executor_send = send;
        let handshake = protocol::Handshake::for_config(&config);
        let admission = AdmissionController::default();
        let (stream, mut peer) = UnixStream::pair().unwrap();

        lane.admit(
            &handshake,
            &admission,
            true,
            rerank_command(now_ms() + 10_000),
            stream,
        );

        assert!(lane.loading);
        assert!(matches!(
            commands.try_recv(),
            Ok(RerankExecutorCommand::Load(_))
        ));
        assert!(commands.try_recv().is_err());

        let deadline = Instant::now() + std::time::Duration::from_millis(500);
        match protocol::read_reply_with_deadline(&mut peer, deadline).unwrap() {
            WorkerReply::Error { message } => assert!(message.contains("rerank_loading")),
            other => panic!("expected Error reply, got {other:?}"),
        }
    }

    #[test]
    fn admit_with_expired_deadline_sends_no_reply_and_closes_the_stream() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let handshake = protocol::Handshake::for_config(&config);
        let admission = AdmissionController::default();
        let (stream, mut peer) = UnixStream::pair().unwrap();

        lane.admit(&handshake, &admission, true, rerank_command(1), stream);

        peer.set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .unwrap();
        let mut buffer = [0_u8; 1];
        assert_eq!(peer.read(&mut buffer).unwrap(), 0);
    }

    #[test]
    fn disconnected_executor_events_channel_fails_load_and_drains_queue() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let (events, receive) = mpsc::channel();
        let (send, _commands) = mpsc::channel();
        lane.executor_receive = receive;
        lane.executor_send = send;
        lane.active = true;
        lane.loaded = true;

        let (queued, mut queued_peer) = queued_rerank(&config, now_ms() + 10_000);
        lane.enqueue(queued);
        drop(events);

        assert!(lane.pump(Instant::now()));
        assert!(!lane.active);
        assert!(!lane.loaded);
        let error = lane.load_error.as_deref().unwrap();
        assert!(error.contains("rerank_unavailable: rerank executor exited"));
        assert!(lane.queue.is_empty());

        let deadline = Instant::now() + std::time::Duration::from_millis(500);
        match protocol::read_reply_with_deadline(&mut queued_peer, deadline).unwrap() {
            WorkerReply::Error { message } => {
                assert!(message.contains("rerank_unavailable: rerank executor exited"))
            }
            other => panic!("expected Error reply, got {other:?}"),
        }

        // A later admit sees the recorded failure and gets an error reply too.
        let handshake = protocol::Handshake::for_config(&config);
        let admission = AdmissionController::default();
        let (stream, mut peer) = UnixStream::pair().unwrap();
        lane.admit(
            &handshake,
            &admission,
            true,
            rerank_command(now_ms() + 10_000),
            stream,
        );
        match protocol::read_reply_with_deadline(&mut peer, deadline).unwrap() {
            WorkerReply::Error { message } => {
                assert!(message.contains("rerank_unavailable: rerank executor exited"))
            }
            other => panic!("expected Error reply, got {other:?}"),
        }
    }

    #[test]
    fn admit_during_retry_cooldown_replies_with_recorded_load_error() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let (_events, receive) = mpsc::channel();
        let (send, commands) = mpsc::channel();
        lane.executor_receive = receive;
        lane.executor_send = send;
        lane.record_load_failure(
            "rerank_unavailable: no reranker configured".into(),
            Instant::now(),
        );

        let handshake = protocol::Handshake::for_config(&config);
        let admission = AdmissionController::default();
        let (stream, mut peer) = UnixStream::pair().unwrap();

        lane.admit(
            &handshake,
            &admission,
            true,
            rerank_command(now_ms() + 10_000),
            stream,
        );

        // Still in cooldown, so no new load should have been started.
        assert!(!lane.loading);
        assert!(commands.try_recv().is_err());

        let deadline = Instant::now() + std::time::Duration::from_millis(500);
        match protocol::read_reply_with_deadline(&mut peer, deadline).unwrap() {
            WorkerReply::Error { message } => {
                assert_eq!(message, "rerank_unavailable: no reranker configured")
            }
            other => panic!("expected Error reply, got {other:?}"),
        }
    }

    #[test]
    fn idle_unload_targets_only_the_rerank_lane() {
        let config = InferenceConfig::default();
        let mut lane = RerankLane::new(config.clone());
        let (events, receive) = mpsc::channel();
        let (send, commands) = mpsc::channel();
        lane.executor_receive = receive;
        lane.executor_send = send;

        events.send(RerankExecutorEvent::Loaded(Ok(()))).unwrap();
        lane.pump(Instant::now());
        assert!(lane.loaded);
        lane.last_used = Some(Instant::now() - super::super::IDLE_UNLOAD);
        lane.unload_idle();

        assert!(!lane.loaded);
        assert!(matches!(
            commands.try_recv(),
            Ok(RerankExecutorCommand::Unload)
        ));
        assert!(commands.try_recv().is_err());
    }
}
