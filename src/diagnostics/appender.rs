use super::{DiagnosticStore, DiagnosticsMode, RecordStatus, RequestEvent, records};
use anyhow::Result;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::Duration;

/// A bounded best-effort append worker; drops are observable on this handle.
pub struct DiagnosticQueue {
    sender: Option<mpsc::SyncSender<RequestEvent>>,
    drops: Arc<AtomicU64>,
    pending_drops: Arc<AtomicU64>,
    mode: DiagnosticsMode,
    completed: mpsc::Receiver<()>,
}

impl DiagnosticQueue {
    pub fn open(cache: &Path, mode: DiagnosticsMode) -> Result<Self> {
        let store = DiagnosticStore::open(cache, mode)?;
        let (sender, receiver) = mpsc::sync_channel::<RequestEvent>(32);
        let drops = Arc::new(AtomicU64::new(0));
        let worker_drops = Arc::clone(&drops);
        let pending_drops = Arc::new(AtomicU64::new(0));
        let worker_pending = Arc::clone(&pending_drops);
        let (done, completed) = mpsc::channel();
        std::thread::spawn(move || {
            for event in receiver {
                store
                    .local_drops
                    .fetch_add(worker_pending.swap(0, Ordering::Relaxed), Ordering::Relaxed);
                let status = store.record(&event);
                if !matches!(status, Ok(RecordStatus::Recorded | RecordStatus::Disabled)) {
                    worker_drops.fetch_add(1, Ordering::Relaxed);
                    if status.is_err() {
                        store.local_drops.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            store
                .local_drops
                .fetch_add(worker_pending.swap(0, Ordering::Relaxed), Ordering::Relaxed);
            if let Ok(Some(_guard)) = store.lock() {
                let _ = store.persist_drops();
            }
            let _ = done.send(());
        });
        Ok(Self {
            sender: Some(sender),
            drops,
            pending_drops,
            mode,
            completed,
        })
    }

    pub fn record(&self, mut event: RequestEvent) -> RecordStatus {
        if self.mode == DiagnosticsMode::Off {
            return RecordStatus::Disabled;
        }
        records::sanitize(&mut event, self.mode);
        if self
            .sender
            .as_ref()
            .is_some_and(|sender| sender.try_send(event).is_ok())
        {
            RecordStatus::Recorded
        } else {
            self.drops.fetch_add(1, Ordering::Relaxed);
            self.pending_drops.fetch_add(1, Ordering::Relaxed);
            RecordStatus::Dropped
        }
    }

    pub fn dropped(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }
}

impl Drop for DiagnosticQueue {
    fn drop(&mut self) {
        self.sender.take();
        let _ = self.completed.recv_timeout(Duration::from_millis(20));
    }
}

/// Includes initialization in the deadline; unfinished work is not delivery evidence.
pub fn best_effort_record(
    cache: &Path,
    mode: DiagnosticsMode,
    event: RequestEvent,
) -> RecordStatus {
    if mode == DiagnosticsMode::Off {
        return RecordStatus::Disabled;
    }
    let cache = cache.to_owned();
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let status = DiagnosticStore::open(&cache, mode).and_then(|store| store.record(&event));
        let _ = sender.send(status.unwrap_or(RecordStatus::Dropped));
    });
    receiver
        .recv_timeout(Duration::from_millis(20))
        .unwrap_or(RecordStatus::Dropped)
}
