//! Shared per-user inference worker and foreground client.

mod admission;
mod client;
mod engine;
mod lease;
mod load_retry;
mod protocol;
mod rerank_executor;
mod scheduler;
mod status;
mod worker_runtime;

pub use admission::{AdmissionController, AdmissionTicket};
pub use protocol::Handshake;
pub use scheduler::{FairScheduler, RequestClass};
pub use status::{WorkerState, WorkerStatus};
use tp_core::coded_error::CodedContext;

use super::Embedding;
use crate::{VerifiedAssetDigests, runtime_config::InferenceConfig};
use anyhow::{Result, bail, ensure};
pub use client::RERANK_DEADLINE;
#[cfg(feature = "semantic")]
pub use client::rerank_query;
use client::{configure_io, connect};
use engine::{Engine, open_engine};
use lease::{WorkerLease, WorkerSocket, worker_cache_dir};
use protocol::{WorkerCommand, WorkerReply, WorkerRequest};
use std::{
    env,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tp_core::background_process::{
    BackgroundOwner, current_background_executable, spawn_background_for,
};
use worker_runtime::WorkerRuntime;

#[cfg(any(test, feature = "semantic"))]
const QUERY_DEADLINE: Duration = Duration::from_millis(500);
const BACKGROUND_DEADLINE: Duration = Duration::from_secs(60);
const CONTROL_DEADLINE: Duration = Duration::from_millis(500);
const WORKER_COMMAND: &str = "semantic-worker-serve";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbedKind {
    #[cfg(feature = "semantic")]
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
            return Err(crate::unavailable());
        }
        let config = InferenceConfig::load()?;
        require_foreground_cuda_mask(&config)?;
        let cache = worker_cache_dir()?;
        let lease = WorkerLease::acquire(&cache)?;
        let engine = open_engine(&config, &cache, &mut VerifiedAssetDigests::default())?;
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
            #[cfg(feature = "semantic")]
            Self::Query => RequestClass::Query,
            Self::Background => RequestClass::Background,
        }
    }
}

/// Start the worker without waiting for model initialization.
pub fn ensure_started(cache_identity: &Path) -> Result<WorkerStatus> {
    #[cfg(not(feature = "semantic"))]
    {
        let _ = cache_identity;
        return Err(crate::unavailable());
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
    let executable = match env::var_os("TRUFFLEPIG_WORKER_BINARY") {
        Some(executable) => PathBuf::from(executable),
        None => current_background_executable(cache, BackgroundOwner::InferenceClient)?,
    };
    let mut command = Command::new(&executable);
    command.arg(WORKER_COMMAND);
    command.env("TRUFFLEPIG_WORKER_CACHE", &cache);
    command.env("TRUFFLEPIG_WORKER_ROOT", cache_identity);
    if let Some(visible) = config.cuda_visible_devices() {
        // A UUID visibility mask makes CUDA ordinals refer to the configured
        // physical devices regardless of host enumeration order; it covers
        // both the embedding and (when distinct) the reranking device.
        command.env("CUDA_VISIBLE_DEVICES", visible);
    }
    if let Some(runtime) = config.runtime_library.as_deref() {
        command.env("ORT_DYLIB_PATH", runtime);
    }
    spawn_background_for(
        &mut command,
        &executable,
        "semantic inference worker",
        cache,
    )?;
    Ok(())
}

/// Return worker state without opening its model.
pub fn status(_: &Path) -> Result<WorkerStatus> {
    #[cfg(not(feature = "semantic"))]
    {
        return Err(crate::unavailable());
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
        return Err(crate::unavailable());
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
        WorkerReply::Error { message } => bail!(coded!(SemanticWorker, "{message}")),
        WorkerReply::Embeddings { .. } | WorkerReply::Scores { .. } => {
            bail!(coded!(SemanticWorker, "invalid stop response"))
        }
    }
}

/// Embed one query through the shared worker, bounded end to end to 500 ms.
#[cfg(feature = "semantic")]
pub fn embed_query(cache_identity: &Path, text: &str) -> Result<Embedding> {
    let results = embed(EmbedKind::Query, cache_identity, &[text.to_owned()])?;
    results
        .into_iter()
        .next()
        .with_coded_context(|| coded!(SemanticWorker, "missing query embedding"))
}

/// Submit a bounded batch to the shared worker.
pub fn embed(kind: EmbedKind, root_id: &Path, texts: &[String]) -> Result<Vec<Embedding>> {
    #[cfg(not(feature = "semantic"))]
    {
        let _ = (kind, root_id, texts);
        return Err(crate::unavailable());
    }
    let started = Instant::now();
    let budget = match kind {
        #[cfg(feature = "semantic")]
        EmbedKind::Query => QUERY_DEADLINE,
        EmbedKind::Background => BACKGROUND_DEADLINE,
    };
    let deadline_instant = started + budget;
    protocol::validate_inputs(texts)?;
    let config = InferenceConfig::load()?;
    let cache = worker_cache_dir()?;
    let remaining = deadline_instant.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!(coded!(
            SemanticTimeout,
            "request exceeded its deadline before send"
        ));
    }
    let deadline_ms = SystemTime::now()
        .checked_add(remaining)
        .with_coded_context(|| coded!(SemanticWorker, "system clock overflow"))?
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
            bail!(coded!(SemanticLoading, "inference worker is starting"))
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
        WorkerReply::Error { message } => bail!(coded!(SemanticWorker, "{message}")),
        WorkerReply::Status(_) | WorkerReply::Scores { .. } => {
            bail!(coded!(SemanticWorker, "invalid embedding response"))
        }
    }
}

/// Run the worker command in the foreground. Both configured models begin
/// loading on their executor threads at startup while the socket serves status.
pub fn serve(cache_override: Option<&Path>) -> Result<()> {
    #[cfg(not(feature = "semantic"))]
    {
        let _ = cache_override;
        return Err(crate::unavailable());
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
    state.start_eager_load(Instant::now());
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
    if config.provider != crate::runtime_config::InferenceProvider::Cuda {
        return Ok(());
    }
    let expected = config
        .cuda_visible_devices()
        .with_coded_context(|| coded!(InferenceConfigInvalid, "cuda requires gpu_uuid"))?;
    let visible = env::var("CUDA_VISIBLE_DEVICES").unwrap_or_default();
    ensure!(
        visible == expected,
        coded!(
            SemanticCuda,
            "foreground inference requires CUDA_VISIBLE_DEVICES={expected}"
        )
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
        coded!(
            SemanticTimeout,
            "request exceeded its deadline while starting worker"
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_constants_preserve_query_budget() {
        assert_eq!(QUERY_DEADLINE, Duration::from_millis(500));
        assert_eq!(RERANK_DEADLINE, Duration::from_millis(1500));
    }
}
