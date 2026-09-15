use super::super::Embedding;
use crate::semantic::runtime_config::{InferenceConfig, InferenceProvider};
use anyhow::Result;
#[cfg(feature = "semantic")]
use anyhow::{Context, bail};
use std::path::Path;

pub trait Engine: Send {
    fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Embedding>>;
}

#[cfg(feature = "semantic")]
struct ModelEngine {
    engine: crate::semantic::SemanticInferenceEngine,
    // Struct fields drop in declaration order, so the model releases its
    // provider before this process-global preload is closed.
    _preload: Option<DynamicLibraryGuard>,
}

#[cfg(feature = "semantic")]
struct DynamicLibraryGuard(*mut libc::c_void);

#[cfg(feature = "semantic")]
unsafe impl Send for DynamicLibraryGuard {}

#[cfg(feature = "semantic")]
impl Drop for DynamicLibraryGuard {
    fn drop(&mut self) {
        // The model has already been dropped because `engine` is declared
        // before `_preload` and Rust drops fields in declaration order.
        unsafe {
            libc::dlclose(self.0);
        }
    }
}

#[cfg(feature = "semantic")]
fn preload_library(path: &Path) -> Result<DynamicLibraryGuard> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow::anyhow!("semantic_unavailable: invalid CUDA preload path"))?;
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        bail!(
            "semantic_unavailable: cannot preload CUDA library {}",
            path.to_string_lossy()
        );
    }
    Ok(DynamicLibraryGuard(handle))
}

#[cfg(feature = "semantic")]
impl Engine for ModelEngine {
    fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Embedding>> {
        self.engine
            .embed_batch_uncached(&texts.iter().map(String::as_str).collect::<Vec<_>>())
    }
}

pub fn open_engine(config: &InferenceConfig, cache: &Path) -> Result<Box<dyn Engine>> {
    config.validate()?;
    match config.provider {
        InferenceProvider::Cpu => open_cpu(config, cache),
        InferenceProvider::Cuda => open_cuda(config, cache),
    }
}

#[cfg(feature = "semantic")]
fn open_cpu(config: &InferenceConfig, _cache: &Path) -> Result<Box<dyn Engine>> {
    let model = config
        .model_dir()
        .context("semantic_worker: model directory is not configured")?;
    let provider =
        crate::semantic::SemanticProviderConfig::cpu().with_arena_max_bytes(config.arena_bytes);
    let engine = match config.runtime_library() {
        Ok(runtime) => {
            crate::semantic::SemanticInferenceEngine::open_with_runtime(model, provider, runtime)?
        }
        Err(_) => crate::semantic::SemanticInferenceEngine::open_with_provider(model, provider)?,
    };
    Ok(Box::new(ModelEngine {
        engine,
        _preload: None,
    }))
}

#[cfg(not(feature = "semantic"))]
fn open_cpu(_: &InferenceConfig, _: &Path) -> Result<Box<dyn Engine>> {
    Err(crate::semantic::unavailable())
}

#[cfg(feature = "semantic")]
fn open_cuda(config: &InferenceConfig, _: &Path) -> Result<Box<dyn Engine>> {
    let ordinal = i32::try_from(
        config
            .cuda_device()?
            .context("cuda device is not configured")?,
    )
    .context("cuda_unavailable: device ordinal does not fit i32")?;
    let model = config
        .model_dir()
        .context("semantic_worker: model directory is not configured")?;
    let provider = crate::semantic::SemanticProviderConfig::cuda(ordinal)
        .with_arena_max_bytes(config.arena_bytes);
    let preload = config
        .cuda_preload_library
        .as_deref()
        .map(preload_library)
        .transpose()?;
    let engine = match config.runtime_library() {
        Ok(runtime) => {
            crate::semantic::SemanticInferenceEngine::open_with_runtime(model, provider, runtime)?
        }
        Err(_) => crate::semantic::SemanticInferenceEngine::open_with_provider(model, provider)?,
    };
    Ok(Box::new(ModelEngine {
        engine,
        _preload: preload,
    }))
}

#[cfg(not(feature = "semantic"))]
fn open_cuda(_: &InferenceConfig, _: &Path) -> Result<Box<dyn Engine>> {
    Err(crate::semantic::unavailable())
}
