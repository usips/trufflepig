use super::*;
use crate::semantic::Embedding;
use crate::store::Store;
use anyhow::{Context, Result};
use std::{
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::TempDir;

fn fixture(source: &str) -> Result<(TempDir, TempDir)> {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("source.rs"), source)?;
    let cache = tempfile::tempdir()?;
    let mut store = Store::open(root.path(), cache.path())?;
    store.index()?;
    Ok((root, cache))
}

fn fake_embedding() -> Embedding {
    let mut values = [0.0; crate::semantic::DIMENSIONS];
    values[0] = 1.0;
    Embedding(values)
}

fn successful_worker()
-> ClosureWorker<impl FnMut(&[EmbeddingInput]) -> Result<Vec<Result<Embedding>>> + Send> {
    ClosureWorker(|batch: &[EmbeddingInput]| {
        Ok(batch.iter().map(|_| Ok(fake_embedding())).collect())
    })
}

#[test]
fn foreground_preparation_persists_content_completion_and_counts() -> Result<()> {
    let (_root, _cache) = fixture("fn one() {}\nfn two() {}\n")?;
    let preparation_status = run_foreground(_root.path(), _cache.path(), successful_worker())?;
    assert_eq!(preparation_status.state, PreparationState::Completed);
    assert_eq!(preparation_status.total, preparation_status.cached);
    assert_eq!(preparation_status.missing, 0);
    assert_eq!(preparation_status.failures, 0);

    let again = super::status(_root.path(), _cache.path())?;
    assert_eq!(again.state, PreparationState::Completed);
    assert_eq!(again.cached, preparation_status.cached);
    Ok(())
}

#[test]
fn failed_units_are_terminal_and_recorded_by_content_key() -> Result<()> {
    let (_root, _cache) = fixture("fn one() {}\n")?;
    let worker = ClosureWorker(|batch: &[EmbeddingInput]| {
        Ok(batch
            .iter()
            .map(|_| Err(anyhow::anyhow!("fake inference failure")))
            .collect())
    });
    let status = run_foreground(_root.path(), _cache.path(), worker)?;
    assert_eq!(status.state, PreparationState::Failed);
    assert_eq!(status.failures, status.total);
    assert_eq!(status.missing, 0);
    Ok(())
}

#[test]
fn manager_coalesces_repeated_schedules_and_waits_for_generation() -> Result<()> {
    let (_root, _cache) = fixture("fn one() {}\nfn two() {}\n")?;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&calls);
    let worker = ClosureWorker(move |batch: &[EmbeddingInput]| {
        observed.lock().unwrap().push(batch.len());
        Ok(batch.iter().map(|_| Ok(fake_embedding())).collect())
    });
    let manager = PreparationManager::new(worker);
    let first = manager.schedule(_root.path(), _cache.path())?;
    let second = manager.schedule(_root.path(), _cache.path())?;
    assert_eq!(first.captured_generation, second.captured_generation);
    assert!(second.coalesced);
    let completed = manager.wait_timeout(
        _root.path(),
        _cache.path(),
        first.captured_generation,
        Duration::from_secs(2),
    )?;
    assert_eq!(completed.state, PreparationState::Completed);
    assert!(calls.lock().unwrap().iter().all(|size| *size <= MAX_BATCH));
    Ok(())
}

struct ColdWorker {
    failures_left: usize,
}

impl EmbeddingWorker for ColdWorker {
    fn embed_batch(&mut self, batch: &[EmbeddingInput]) -> Result<Vec<Result<Embedding>>> {
        if self.failures_left != 0 {
            let error = if self.failures_left == 2 {
                "semantic_loading: fake cold worker"
            } else {
                "semantic_busy: fake cold worker"
            };
            self.failures_left -= 1;
            return Err(anyhow::anyhow!(error));
        }
        Ok(batch.iter().map(|_| Ok(fake_embedding())).collect())
    }

    fn is_retryable_batch_error(&self, error: &anyhow::Error) -> bool {
        let message = error.to_string();
        message.contains("semantic_loading") || message.contains("semantic_busy")
    }
}

#[test]
fn retryable_cold_worker_errors_do_not_poison_content() -> Result<()> {
    let (_root, _cache) = fixture("fn one() {}\n")?;
    let status = run_foreground(_root.path(), _cache.path(), ColdWorker { failures_left: 2 })?;
    assert_eq!(status.state, PreparationState::Completed);
    assert_eq!(status.failures, 0);
    assert_eq!(status.missing, 0);

    let preparation = cache::PreparationCache::open(_cache.path())?;
    assert_eq!(
        preparation.completion(&content_key("fn one() {}\n"))?,
        Some(true)
    );
    Ok(())
}

struct SplittingWorker;

impl EmbeddingWorker for SplittingWorker {
    fn embed_batch(&mut self, batch: &[EmbeddingInput]) -> Result<Vec<Result<Embedding>>> {
        if batch.iter().any(|input| input.content_key == "bad") {
            return Err(anyhow::anyhow!(
                "semantic_unavailable: invalid model output"
            ));
        }
        Ok(batch.iter().map(|_| Ok(fake_embedding())).collect())
    }

    fn batch_error_is_unit_failure(&self, _: &anyhow::Error) -> bool {
        false
    }
}

#[test]
fn permanent_batch_errors_are_split_to_the_bad_input() -> Result<()> {
    let inputs = ["good-a", "bad", "good-b"]
        .into_iter()
        .map(|content_key| EmbeddingInput {
            content_key: content_key.into(),
            text: content_key.into(),
            token_count: 1,
        })
        .collect::<Vec<_>>();
    let mut worker = SplittingWorker;
    let mut outcomes = Vec::new();
    worker::run_batches_grouped(&mut worker, &inputs, |batch, results| {
        for (input, result) in batch.iter().zip(results) {
            outcomes.push((input.content_key.clone(), result.is_ok()));
        }
        Ok(())
    })?;
    assert_eq!(
        outcomes,
        [
            ("good-a".into(), true),
            ("bad".into(), false),
            ("good-b".into(), true)
        ]
    );
    Ok(())
}

#[test]
fn wait_reports_a_captured_generation_as_superseded() -> Result<()> {
    let (root, cache) = fixture("fn one() {}\n")?;
    let receipt = schedule(root.path(), cache.path())?;
    fs::write(root.path().join("source.rs"), "fn one() {}\nfn two() {}\n")?;
    let mut store = Store::open(root.path(), cache.path())?;
    store.index()?;
    let waited = wait_timeout(
        root.path(),
        cache.path(),
        receipt.captured_generation,
        Duration::from_millis(100),
    )?;
    assert_eq!(waited.state, PreparationState::Superseded);
    assert_eq!(waited.captured_generation, receipt.captured_generation);
    Ok(())
}

#[test]
fn completion_metadata_is_pruned_to_current_content() -> Result<()> {
    let (root, cache) = fixture("fn one() {}\n")?;
    let mut preparation = cache::PreparationCache::open(cache.path())?;
    preparation.fail("orphan-content", "stale")?;
    preparation.prune_completions([content_key("fn one() {}\n")])?;
    assert_eq!(preparation.completion("orphan-content")?, None);
    drop(preparation);
    let status = run_foreground(root.path(), cache.path(), successful_worker())?;
    assert_eq!(status.state, PreparationState::Completed);
    Ok(())
}

#[test]
fn running_run_restarts_from_its_persisted_cursor() -> Result<()> {
    let cache_dir = tempfile::tempdir()?;
    let mut preparation = cache::PreparationCache::open(cache_dir.path())?;
    let started = preparation.begin(7, 300, false)?;
    assert_eq!(started.cursor, 0);
    preparation.progress(7, 256, 300, 250, 5, 1)?;
    drop(preparation);

    let mut reopened = cache::PreparationCache::open(cache_dir.path())?;
    let resumed = reopened.begin(7, 300, false)?;
    assert_eq!(resumed.state, PreparationState::Running);
    assert_eq!(resumed.cursor, 256);
    assert_eq!(resumed.cached, 250);
    assert_eq!(resumed.missing, 5);
    assert_eq!(resumed.failures, 1);
    Ok(())
}

#[test]
fn worker_error_upserts_a_failed_run_before_begin() -> Result<()> {
    let cache_dir = tempfile::tempdir()?;
    let mut preparation = cache::PreparationCache::open(cache_dir.path())?;
    preparation.record_worker_error(9, "fake worker setup failure")?;
    let failed = preparation.run(9)?.context("worker error run missing")?;
    assert_eq!(failed.state, PreparationState::Failed);
    assert_eq!(failed.failures, 1);
    assert_eq!(failed.error.as_deref(), Some("fake worker setup failure"));

    assert!(preparation.reset_terminal(9)?);
    assert_eq!(
        preparation.run(9)?.unwrap().state,
        PreparationState::Running
    );
    Ok(())
}

#[test]
fn completed_run_reports_capacity_after_vector_eviction() -> Result<()> {
    let cache_dir = tempfile::tempdir()?;
    let mut preparation = cache::PreparationCache::open(cache_dir.path())?;
    preparation.begin(11, 1, false)?;
    preparation.finish(11, PreparationState::Completed, 1, 1, 1, 0, 0, None)?;
    let status = preparation.counts_for_status(11, 1, 0, 1, 0)?;
    assert_eq!(status.state, PreparationState::Capacity);
    assert_eq!(
        status.error.as_deref(),
        Some("embedding cache capacity cannot retain all content")
    );
    Ok(())
}

#[test]
fn only_explicit_retry_reopens_a_failed_generation() -> Result<()> {
    let (root, cache) = fixture("fn one() {}\n")?;
    let worker = ClosureWorker(|batch: &[EmbeddingInput]| {
        Ok(batch
            .iter()
            .map(|_| Err(anyhow::anyhow!("permanent input failure")))
            .collect())
    });
    assert_eq!(
        run_foreground(root.path(), cache.path(), worker)?.state,
        PreparationState::Failed
    );

    let scheduled = schedule(root.path(), cache.path())?;
    assert!(!scheduled.coalesced);
    assert_eq!(
        status(root.path(), cache.path())?.state,
        PreparationState::Failed
    );

    let retried = retry(root.path(), cache.path())?;
    assert!(!retried.coalesced);
    assert_eq!(
        status(root.path(), cache.path())?.state,
        PreparationState::Running
    );

    let completed = run_foreground(root.path(), cache.path(), successful_worker())?;
    assert_eq!(completed.state, PreparationState::Completed);
    assert_eq!(completed.failures, 0);
    assert_eq!(completed.missing, 0);
    Ok(())
}

#[test]
fn explicit_retry_refills_missing_vectors_after_terminal_run() -> Result<()> {
    let (root, cache) = fixture("fn one() {}\n")?;
    let source_key = content_key("fn one() {}\n");
    let completed = run_foreground(root.path(), cache.path(), successful_worker())?;
    assert_eq!(completed.state, PreparationState::Completed);

    let preparation = cache::PreparationCache::open(cache.path())?;
    preparation.db.execute(
        "DELETE FROM preparation_completions WHERE content_key=?1",
        [&source_key],
    )?;
    drop(preparation);
    #[cfg(feature = "semantic")]
    {
        let embeddings = rusqlite::Connection::open(cache.path().join("embeddings.sqlite"))?;
        embeddings.execute("DELETE FROM embeddings WHERE key=?1", [&source_key])?;
        embeddings.execute(
            "DELETE FROM embedding_provenance WHERE key=?1",
            [&source_key],
        )?;
    }

    let ordinary = schedule(root.path(), cache.path())?;
    assert!(!ordinary.coalesced);
    assert_eq!(
        status(root.path(), cache.path())?.state,
        PreparationState::Capacity
    );
    let repeated = schedule(root.path(), cache.path())?;
    assert!(repeated.coalesced);
    assert_eq!(
        status(root.path(), cache.path())?.state,
        PreparationState::Capacity
    );

    let retried = retry(root.path(), cache.path())?;
    assert!(!retried.coalesced);
    assert_eq!(
        status(root.path(), cache.path())?.state,
        PreparationState::Running
    );
    let refilled = run_foreground(root.path(), cache.path(), successful_worker())?;
    assert_eq!(refilled.state, PreparationState::Completed);
    assert_eq!(refilled.cached, refilled.total);
    assert_eq!(refilled.missing, 0);
    Ok(())
}

#[test]
fn content_keys_change_when_region_text_changes() {
    assert_ne!(content_key("old"), content_key("new"));
    assert_eq!(content_key("same"), content_key("same"));
}

#[test]
fn foreground_prepare_retries_failed_content_directly() -> Result<()> {
    let (root, cache) = fixture("fn retry_me() {}\n")?;
    let failed = ClosureWorker(|batch: &[EmbeddingInput]| {
        Ok(batch
            .iter()
            .map(|_| Err(anyhow::anyhow!("bad input")))
            .collect())
    });
    assert_eq!(
        run_foreground(root.path(), cache.path(), failed)?.state,
        PreparationState::Failed
    );
    let retried = run_foreground(root.path(), cache.path(), successful_worker())?;
    assert_eq!(retried.state, PreparationState::Completed);
    assert_eq!(retried.failures, 0);
    assert_eq!(retried.missing, 0);
    Ok(())
}

#[test]
fn retry_during_running_preparation_coalesces_after_completion() -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (root, cache) = fixture("fn one() {}\n")?;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let (started, start) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let worker = ClosureWorker(move |batch: &[EmbeddingInput]| {
        if observed.fetch_add(1, Ordering::SeqCst) == 0 {
            started.send(())?;
            released.recv_timeout(Duration::from_secs(5))?;
        }
        Ok(batch
            .iter()
            .map(|_| Err(anyhow::anyhow!("permanent input failure")))
            .collect())
    });
    let mut manager = PreparationManager::new(worker);
    let initial = manager.schedule(root.path(), cache.path())?;
    start.recv_timeout(Duration::from_secs(5))?;
    let retry = manager.retry(root.path(), cache.path())?;
    assert!(retry.coalesced);
    release.send(())?;
    manager.wait_timeout(
        root.path(),
        cache.path(),
        initial.captured_generation,
        Duration::from_secs(5),
    )?;
    assert!(manager.commands.send(Command::Stop).is_ok());
    manager.thread.take().unwrap().join().unwrap();
    drop(manager);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        status(root.path(), cache.path())?.state,
        PreparationState::Failed
    );
    Ok(())
}
