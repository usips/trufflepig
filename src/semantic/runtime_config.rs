//! User-wide inference runtime configuration.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

mod cuda_identity;

pub const DEFAULT_ARENA_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const CONFIG_DIRECTORY: &str = ".config/trufflepig";
const CONFIG_FILE: &str = "inference.toml";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InferenceProvider {
    #[default]
    Cpu,
    Cuda,
}

impl std::fmt::Display for InferenceProvider {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InferenceConfig {
    pub provider: InferenceProvider,
    pub gpu_uuid: Option<String>,
    pub model_dir: Option<PathBuf>,
    pub runtime_library: Option<PathBuf>,
    pub cuda_preload_library: Option<PathBuf>,
    pub arena_bytes: u64,
    pub rerank_model_dir: Option<PathBuf>,
    pub rerank_gpu_uuid: Option<String>,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            provider: InferenceProvider::Cpu,
            gpu_uuid: None,
            model_dir: None,
            runtime_library: None,
            cuda_preload_library: None,
            arena_bytes: DEFAULT_ARENA_BYTES,
            rerank_model_dir: None,
            rerank_gpu_uuid: None,
        }
    }
}

impl InferenceConfig {
    pub fn path() -> Result<PathBuf> {
        if let Some(path) = env::var_os("TRUFFLEPIG_INFERENCE_CONFIG") {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                bail!("inference_config_invalid: TRUFFLEPIG_INFERENCE_CONFIG must be absolute");
            }
            return Ok(path);
        }
        let home = env::var_os("HOME").context("inference_config_unavailable: HOME is unset")?;
        Ok(PathBuf::from(home).join(CONFIG_DIRECTORY).join(CONFIG_FILE))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        Self::from_path(&path)
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        let mut config = match fs::read_to_string(path) {
            Ok(contents) => toml::from_str::<Self>(&contents)
                .with_context(|| format!("invalid inference config {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read inference config {}", path.display()));
            }
        };
        config.apply_environment_defaults();
        config.validate()?;
        Ok(config)
    }

    pub fn apply_environment_defaults(&mut self) {
        if self.model_dir.is_none() {
            self.model_dir = env::var_os("TRUFFLEPIG_MODEL_DIR").map(PathBuf::from);
        }
        if self.runtime_library.is_none() {
            self.runtime_library = env::var_os("ORT_DYLIB_PATH").map(PathBuf::from);
        }
        if self.cuda_preload_library.is_none() {
            self.cuda_preload_library =
                env::var_os("TRUFFLEPIG_CUDA_PRELOAD_LIBRARY").map(PathBuf::from);
        }
        if self.rerank_model_dir.is_none() {
            self.rerank_model_dir = env::var_os("TRUFFLEPIG_RERANK_MODEL_DIR").map(PathBuf::from);
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.arena_bytes == 0 {
            bail!("inference_config_invalid: arena_bytes must be positive");
        }
        if self.provider == InferenceProvider::Cuda
            && self.gpu_uuid.as_deref().is_none_or(str::is_empty)
        {
            bail!("inference_config_invalid: cuda requires gpu_uuid");
        }
        if self.rerank_gpu_uuid.is_some() && self.provider != InferenceProvider::Cuda {
            bail!("inference_config_invalid: rerank_gpu_uuid requires the cuda provider");
        }
        if self.rerank_gpu_uuid.is_some() && self.rerank_model_dir.is_none() {
            bail!("inference_config_invalid: rerank_gpu_uuid requires rerank_model_dir");
        }
        Ok(())
    }

    pub fn model_dir(&self) -> Result<&Path> {
        self.model_dir.as_deref().context(
            "semantic_unavailable: set model_dir in inference.toml or TRUFFLEPIG_MODEL_DIR",
        )
    }

    pub fn runtime_library(&self) -> Result<&Path> {
        self.runtime_library.as_deref().context(
            "semantic_unavailable: set runtime_library in inference.toml or ORT_DYLIB_PATH",
        )
    }

    /// Resolve the configured UUID to the current CUDA ordinal at process start.
    pub fn cuda_device(&self) -> Result<Option<u32>> {
        if self.provider != InferenceProvider::Cuda {
            return Ok(None);
        }
        let wanted = self.gpu_uuid.as_deref().context("cuda requires gpu_uuid")?;
        Ok(Some(resolve_cuda_uuid(wanted)?))
    }

    /// True when a reranker model directory is configured.
    pub fn rerank_enabled(&self) -> bool {
        self.rerank_model_dir.is_some()
    }

    pub fn rerank_model_dir(&self) -> Result<&Path> {
        self.rerank_model_dir.as_deref().context(
            "rerank_unavailable: set rerank_model_dir in inference.toml or TRUFFLEPIG_RERANK_MODEL_DIR",
        )
    }

    /// Resolve the configured reranker UUID to its CUDA ordinal, falling back
    /// to the shared `gpu_uuid` when `rerank_gpu_uuid` is unset.
    pub fn rerank_cuda_device(&self) -> Result<Option<u32>> {
        if self.provider != InferenceProvider::Cuda {
            return Ok(None);
        }
        let wanted = self
            .rerank_gpu_uuid
            .as_deref()
            .or(self.gpu_uuid.as_deref())
            .context("cuda requires gpu_uuid")?;
        Ok(Some(resolve_cuda_uuid(wanted)?))
    }

    /// Comma-joined CUDA device UUIDs the process should expose, covering
    /// both the embedding and reranking devices when they differ. `None` on
    /// the CPU provider.
    pub fn cuda_visible_devices(&self) -> Option<String> {
        if self.provider != InferenceProvider::Cuda {
            return None;
        }
        let primary = self.gpu_uuid.as_deref()?;
        match self.rerank_gpu_uuid.as_deref() {
            Some(rerank) if rerank != primary => Some(format!("{primary},{rerank}")),
            _ => Some(primary.to_owned()),
        }
    }

    pub fn fingerprint(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("inference config is serializable");
        blake3::hash(&bytes).to_hex().to_string()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CudaDevice {
    pub uuid: String,
    pub ordinal: u32,
}

pub fn resolve_cuda_uuid(wanted: &str) -> Result<u32> {
    cuda_identity::resolve_cuda_uuid(wanted)
}

pub fn enumerate_cuda_devices() -> Result<Vec<CudaDevice>> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=uuid,index", "--format=csv,noheader,nounits"])
        .output()
        .context("cuda_unavailable: nvidia-smi is unavailable")?;
    if !output.status.success() {
        bail!(
            "cuda_unavailable: nvidia-smi failed with status {}",
            output.status
        );
    }
    let mut devices = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split(',').map(str::trim);
        let uuid = fields.next().unwrap_or_default();
        let ordinal = fields.next().unwrap_or_default().parse::<u32>();
        if uuid.is_empty() || ordinal.is_err() {
            bail!("cuda_unavailable: malformed nvidia-smi GPU row");
        }
        devices.push(CudaDevice {
            uuid: uuid.to_owned(),
            ordinal: ordinal.unwrap_or_default(),
        });
    }
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_cpu_with_ten_gib_arena() {
        let config = InferenceConfig::default();
        assert_eq!(config.provider, InferenceProvider::Cpu);
        assert_eq!(config.arena_bytes, DEFAULT_ARENA_BYTES);
    }

    #[test]
    fn config_toml_round_trips_optional_runtime_paths() {
        let config = InferenceConfig {
            provider: InferenceProvider::Cuda,
            gpu_uuid: Some("GPU-test".into()),
            model_dir: Some("models".into()),
            runtime_library: Some("onnxruntime.so".into()),
            cuda_preload_library: Some("libcudnn.so.9".into()),
            arena_bytes: 123,
            rerank_model_dir: Some("reranker".into()),
            rerank_gpu_uuid: Some("GPU-rerank".into()),
        };
        let parsed: InferenceConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn cuda_requires_a_uuid() {
        let config = InferenceConfig {
            provider: InferenceProvider::Cuda,
            ..Default::default()
        };
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("gpu_uuid")
        );
    }

    #[test]
    fn rerank_gpu_uuid_requires_cuda_provider() {
        let config = InferenceConfig {
            provider: InferenceProvider::Cpu,
            rerank_gpu_uuid: Some("GPU-rerank".into()),
            ..Default::default()
        };
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("rerank_gpu_uuid")
        );
    }

    #[test]
    fn rerank_gpu_uuid_requires_rerank_model_dir() {
        let config = InferenceConfig {
            provider: InferenceProvider::Cuda,
            gpu_uuid: Some("GPU-test".into()),
            rerank_gpu_uuid: Some("GPU-rerank".into()),
            rerank_model_dir: None,
            ..Default::default()
        };
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("rerank_model_dir")
        );
    }

    #[test]
    fn cuda_visible_devices_is_none_for_cpu() {
        let config = InferenceConfig::default();
        assert_eq!(config.cuda_visible_devices(), None);
    }

    #[test]
    fn cuda_visible_devices_joins_distinct_uuids() {
        let config = InferenceConfig {
            provider: InferenceProvider::Cuda,
            gpu_uuid: Some("GPU-a".into()),
            rerank_gpu_uuid: Some("GPU-b".into()),
            ..Default::default()
        };
        assert_eq!(
            config.cuda_visible_devices(),
            Some("GPU-a,GPU-b".to_string())
        );
    }

    #[test]
    fn cuda_visible_devices_collapses_matching_uuids() {
        let config = InferenceConfig {
            provider: InferenceProvider::Cuda,
            gpu_uuid: Some("GPU-a".into()),
            rerank_gpu_uuid: Some("GPU-a".into()),
            ..Default::default()
        };
        assert_eq!(config.cuda_visible_devices(), Some("GPU-a".to_string()));
    }

    #[test]
    fn cuda_visible_devices_defaults_to_shared_uuid_without_rerank_override() {
        let config = InferenceConfig {
            provider: InferenceProvider::Cuda,
            gpu_uuid: Some("GPU-a".into()),
            ..Default::default()
        };
        assert_eq!(config.cuda_visible_devices(), Some("GPU-a".to_string()));
    }

    #[test]
    fn rerank_model_dir_reports_actionable_error_when_unset() {
        let config = InferenceConfig::default();
        assert!(!config.rerank_enabled());
        let error = config.rerank_model_dir().unwrap_err().to_string();
        assert!(error.contains("rerank_unavailable"));
        assert!(error.contains("TRUFFLEPIG_RERANK_MODEL_DIR"));
    }
}
