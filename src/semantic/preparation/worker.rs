use super::{EmbeddingInput, MAX_BATCH, MAX_MODEL_TOKENS, MAX_PADDED_TOKENS};
use crate::semantic::Embedding;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// A caller-owned inference worker. Calls are serialized by the preparation thread.
pub trait EmbeddingWorker: Send {
    fn embed_batch(&mut self, inputs: &[EmbeddingInput]) -> Result<Vec<Result<Embedding>>>;

    fn is_retryable_batch_error(&self, _: &anyhow::Error) -> bool {
        false
    }

    fn batch_error_is_unit_failure(&self, _: &anyhow::Error) -> bool {
        true
    }
}

/// Adapts a mutable closure to [`EmbeddingWorker`], useful for tests and probes.
pub struct ClosureWorker<F>(pub F);

impl<F> EmbeddingWorker for ClosureWorker<F>
where
    F: FnMut(&[EmbeddingInput]) -> Result<Vec<Result<Embedding>>> + Send,
{
    fn embed_batch(&mut self, inputs: &[EmbeddingInput]) -> Result<Vec<Result<Embedding>>> {
        (self.0)(inputs)
    }

    fn is_retryable_batch_error(&self, error: &anyhow::Error) -> bool {
        is_runtime_batch_error(error)
    }
}

/// Uses the shared process worker for background batches.
pub struct SharedWorker {
    root_id: PathBuf,
}

/// Adapts the local foreground session for `--no-daemon` preparation.
pub struct ForegroundWorker {
    session: crate::semantic::worker::ForegroundSession,
}

impl ForegroundWorker {
    pub fn open(root_id: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            session: crate::semantic::worker::ForegroundSession::open(root_id.as_ref())?,
        })
    }
}

impl EmbeddingWorker for ForegroundWorker {
    fn embed_batch(&mut self, inputs: &[EmbeddingInput]) -> Result<Vec<Result<Embedding>>> {
        let texts = inputs
            .iter()
            .map(|input| input.text.clone())
            .collect::<Vec<_>>();
        match self.session.embed_batch(&texts) {
            Ok(vectors) if vectors.len() == inputs.len() => {
                Ok(vectors.into_iter().map(Ok).collect())
            }
            Ok(vectors) => Err(anyhow::anyhow!(
                "preparation_worker_failed: foreground returned {} vectors for {} inputs",
                vectors.len(),
                inputs.len()
            )),
            Err(error) => Err(error),
        }
    }

    fn batch_error_is_unit_failure(&self, _: &anyhow::Error) -> bool {
        false
    }

    fn is_retryable_batch_error(&self, error: &anyhow::Error) -> bool {
        is_runtime_batch_error(error)
    }
}

impl SharedWorker {
    pub fn new(root_id: impl AsRef<Path>) -> Self {
        Self {
            root_id: root_id.as_ref().to_owned(),
        }
    }
}

impl EmbeddingWorker for SharedWorker {
    fn embed_batch(&mut self, inputs: &[EmbeddingInput]) -> Result<Vec<Result<Embedding>>> {
        let texts = inputs
            .iter()
            .map(|input| input.text.clone())
            .collect::<Vec<_>>();
        match crate::semantic::worker::embed(
            crate::semantic::worker::EmbedKind::Background,
            &self.root_id,
            &texts,
        ) {
            Ok(vectors) if vectors.len() == inputs.len() => {
                Ok(vectors.into_iter().map(Ok).collect())
            }
            Ok(vectors) => Err(anyhow::anyhow!(
                "preparation_worker_failed: worker returned {} vectors for {} inputs",
                vectors.len(),
                inputs.len()
            )),
            Err(error) => Err(error),
        }
    }

    fn is_retryable_batch_error(&self, error: &anyhow::Error) -> bool {
        is_runtime_batch_error(error)
    }

    fn batch_error_is_unit_failure(&self, _: &anyhow::Error) -> bool {
        false
    }
}

#[cfg(test)]
pub(super) fn run_batches<W: EmbeddingWorker>(
    worker: &mut W,
    inputs: &[EmbeddingInput],
    mut complete: impl FnMut(&EmbeddingInput, Result<Embedding>) -> Result<()>,
) -> Result<()> {
    run_batches_grouped(worker, inputs, |batch, results| {
        for (input, result) in batch.iter().zip(results) {
            complete(input, result)?;
        }
        Ok(())
    })
}

pub(super) fn run_batches_grouped<W: EmbeddingWorker>(
    worker: &mut W,
    inputs: &[EmbeddingInput],
    mut complete: impl FnMut(&[EmbeddingInput], Vec<Result<Embedding>>) -> Result<()>,
) -> Result<()> {
    let mut start = 0;
    while start < inputs.len() {
        let end = next_batch_end(inputs, start);
        let batch = &inputs[start..end];
        run_one_batch(worker, batch, &mut complete)?;
        start = end;
    }
    Ok(())
}

fn run_one_batch<W: EmbeddingWorker>(
    worker: &mut W,
    batch: &[EmbeddingInput],
    complete: &mut impl FnMut(&[EmbeddingInput], Vec<Result<Embedding>>) -> Result<()>,
) -> Result<()> {
    let results = match worker.embed_batch(batch) {
        Ok(results) if results.len() == batch.len() => results,
        Ok(results) => {
            let error = anyhow::anyhow!(
                "preparation_worker_failed: worker returned {} results for {} inputs",
                results.len(),
                batch.len()
            );
            return handle_batch_error(worker, batch, error, complete);
        }
        Err(error) => return handle_batch_error(worker, batch, error, complete),
    };
    complete(batch, results)
}

fn handle_batch_error<W: EmbeddingWorker>(
    worker: &mut W,
    batch: &[EmbeddingInput],
    error: anyhow::Error,
    complete: &mut impl FnMut(&[EmbeddingInput], Vec<Result<Embedding>>) -> Result<()>,
) -> Result<()> {
    if worker.is_retryable_batch_error(&error) {
        return Err(error);
    }
    if worker.batch_error_is_unit_failure(&error) || batch.len() == 1 {
        let message = error.to_string();
        return complete(
            batch,
            (0..batch.len())
                .map(|_| Err(anyhow::anyhow!(message.clone())))
                .collect(),
        );
    }

    // Shared and foreground workers expose batch-level model errors. A bad
    // input must not turn the whole source generation into failed content, so
    // bisect the bounded batch until the offending input is isolated.
    let middle = batch.len() / 2;
    run_one_batch(worker, &batch[..middle], complete)?;
    run_one_batch(worker, &batch[middle..], complete)
}

fn is_runtime_batch_error(error: &anyhow::Error) -> bool {
    if error.chain().any(|cause| cause.is::<std::io::Error>()) {
        return true;
    }
    let message = format!("{error:#}").to_ascii_lowercase();
    // These errors describe a worker, provider, or runtime that is unavailable
    // for the whole request. Input/model errors below are deliberately allowed
    // to recurse and become durable per-input outcomes.
    let input_failure = [
        "input at batch index",
        "inference failed for batch inputs",
        "model returned",
        "invalid model output",
        "raw inputs exceed",
    ]
    .iter()
    .any(|marker| message.contains(marker));
    if input_failure {
        return false;
    }
    [
        "semantic_loading",
        "semantic_busy",
        "semantic_root_busy",
        "semantic_worker_busy",
        "semantic_worker_unavailable",
        "semantic_worker:",
        "semantic_admission",
        "handshake_mismatch",
        "protocol:",
        "busy",
        "disconnected",
        "unexpected end of worker frame",
        "connection reset",
        "broken pipe",
        "no such file",
        "deadline",
        "timed out",
        "timeout",
        "connection refused",
        "worker is starting",
        "runtime unavailable",
        "runtime_unavailable",
        "provider unavailable",
        "cuda_unavailable",
        "inference_config",
        "semantic_unavailable",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

pub(super) fn next_batch_end(inputs: &[EmbeddingInput], start: usize) -> usize {
    let mut end = start;
    let mut max_tokens = 0;
    while end < inputs.len() && end - start < MAX_BATCH {
        let tokens = inputs[end].token_count;
        if tokens > MAX_MODEL_TOKENS {
            // Let the caller record an input-specific failure, while keeping the
            // oversized input as a single outstanding batch.
            return end + 1;
        }
        let next_max = max_tokens.max(tokens);
        if end > start && next_max.saturating_mul(end + 1 - start) > MAX_PADDED_TOKENS {
            break;
        }
        max_tokens = next_max;
        end += 1;
    }
    end.max(start + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn transport_causes_remain_retryable_through_context() {
        let timeout = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::TimedOut))
            .context("read worker frame length");
        assert!(is_runtime_batch_error(&timeout));
        let eof =
            anyhow::anyhow!("unexpected end of worker frame").context("read worker frame length");
        assert!(is_runtime_batch_error(&eof));
        let input = anyhow::anyhow!("input at batch index 0 exceeds model tokens")
            .context("semantic_unavailable");
        assert!(!is_runtime_batch_error(&input));
    }

    fn input(tokens: usize) -> EmbeddingInput {
        EmbeddingInput {
            content_key: tokens.to_string(),
            text: String::new(),
            token_count: tokens,
        }
    }

    #[test]
    fn batches_obey_count_and_padded_token_limits() {
        let inputs = (0..20).map(|_| input(4096)).collect::<Vec<_>>();
        let mut starts = Vec::new();
        let mut start = 0;
        while start < inputs.len() {
            let end = next_batch_end(&inputs, start);
            starts.push(end - start);
            start = end;
        }
        assert_eq!(starts, vec![2; 10]);
        assert!(starts.iter().all(|size| *size <= MAX_BATCH));
        assert!(starts.iter().all(|size| size * 4096 <= MAX_PADDED_TOKENS));
    }

    #[test]
    fn closure_worker_receives_each_subdivided_batch() -> Result<()> {
        let mut seen = Vec::new();
        let mut worker = ClosureWorker(|batch: &[EmbeddingInput]| {
            seen.push(batch.len());
            Ok(batch
                .iter()
                .map(|_| Ok(Embedding([1.0; crate::semantic::DIMENSIONS])))
                .collect())
        });
        let inputs = (0..17).map(|_| input(1)).collect::<Vec<_>>();
        run_batches(&mut worker, &inputs, |_, result| result.map(|_| ()))?;
        assert_eq!(seen, [8, 8, 1]);
        Ok(())
    }
}
