//! Worker status snapshot shared by control commands and CLI output.

use super::protocol::Handshake;
use crate::semantic::runtime_config::InferenceConfig;

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
    // `#[serde(default)]` on these rerank fields lets a newer client decode a
    // `Status` frame from an older worker that predates them, so it never
    // mistakes a live lease for a dead one and spawns a second worker over it.
    #[serde(default)]
    pub rerank_loaded: bool,
    #[serde(default)]
    pub rerank_gpu_uuid: Option<String>,
    #[serde(default)]
    pub rerank_error: Option<String>,
    #[serde(default)]
    pub rerank_pending: usize,
}

impl WorkerStatus {
    pub(super) fn not_running(config: &InferenceConfig) -> Self {
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
            rerank_loaded: false,
            rerank_gpu_uuid: config
                .rerank_enabled()
                .then(|| {
                    config
                        .rerank_gpu_uuid
                        .clone()
                        .or_else(|| config.gpu_uuid.clone())
                })
                .flatten(),
            rerank_error: None,
            rerank_pending: 0,
        }
    }

    pub(super) fn loading(config: &InferenceConfig) -> Self {
        let mut status = Self::not_running(config);
        status.state = WorkerState::Loading;
        status
    }
}
