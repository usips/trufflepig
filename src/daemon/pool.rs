//! Fixed worker threads draining a bounded request queue: a full queue refuses
//! work instead of letting accepted connections wait unread. A panicking job
//! never takes its worker down.
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// One unit of daemon work: read a request, dispatch it, write its reply.
pub(crate) type PooledJob = Box<dyn FnOnce() + Send + 'static>;

/// Worker and queue sizes for one daemon tier.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PoolSize {
    pub workers: usize,
    pub queue: usize,
}

/// Router: many clients, each request waits on a coordinator or root daemon.
pub(crate) const ROUTER_POOL: PoolSize = PoolSize {
    workers: 16,
    queue: 64,
};
/// Workspace coordinator: fans each query out across members.
pub(crate) const COORDINATOR_POOL: PoolSize = PoolSize {
    workers: 8,
    queue: 64,
};
/// Per-root daemon: query-only reads beside the reconciler's writer.
pub(crate) const ROOT_POOL: PoolSize = PoolSize {
    workers: 4,
    queue: 64,
};

/// Threads sharing one bounded queue; dropping the pool finishes queued work
/// and joins every worker.
pub(crate) struct RequestPool {
    sender: Option<mpsc::SyncSender<PooledJob>>,
    workers: Vec<JoinHandle<()>>,
    /// Jobs queued or running.
    pending: Arc<AtomicUsize>,
}

impl RequestPool {
    pub fn new(name: &str, size: PoolSize) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<PooledJob>(size.queue);
        let receiver = Arc::new(Mutex::new(receiver));
        let pending = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::with_capacity(size.workers);
        for index in 0..size.workers.max(1) {
            let (receiver, pending) = (Arc::clone(&receiver), Arc::clone(&pending));
            workers.push(
                std::thread::Builder::new()
                    .name(format!("{name}-{index}"))
                    .spawn(move || {
                        loop {
                            // The guard drops at the end of this statement, so
                            // jobs run without holding the queue lock.
                            let job = receiver.lock().map(|queue| queue.recv());
                            let Ok(Ok(job)) = job else {
                                return;
                            };
                            // Jobs answer their own panics; this keeps the worker.
                            let _ = catch_unwind(AssertUnwindSafe(job));
                            pending.fetch_sub(1, Ordering::AcqRel);
                        }
                    })?,
            );
        }
        Ok(Self {
            sender: Some(sender),
            workers,
            pending,
        })
    }

    /// Queues `job`, handing it back when every worker is busy and the queue is full.
    pub fn submit(&self, job: PooledJob) -> Result<(), PooledJob> {
        let sender = self
            .sender
            .as_ref()
            .expect("pool accepts work until dropped");
        self.pending.fetch_add(1, Ordering::AcqRel);
        match sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job)) => {
                self.pending.fetch_sub(1, Ordering::AcqRel);
                Err(job)
            }
        }
    }

    /// Waits up to `limit` for every queued and running job to finish.
    pub fn drain(&self, limit: Duration) -> bool {
        let started = Instant::now();
        while self.pending.load(Ordering::Acquire) > 0 {
            if started.elapsed() >= limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }
}

impl Drop for RequestPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn request_pool_runs_jobs_in_parallel_and_refuses_when_full() {
        let pool = RequestPool::new(
            "test-pool",
            PoolSize {
                workers: 2,
                queue: 1,
            },
        )
        .unwrap();
        let started = Arc::new(Barrier::new(3));
        let release = Arc::new(Barrier::new(3));
        for _ in 0..2 {
            let (started, release) = (Arc::clone(&started), Arc::clone(&release));
            let mut job: PooledJob = Box::new(move || {
                started.wait();
                release.wait();
            });
            // Until a worker is waiting, the one-slot queue may still hold the first job.
            while let Err(refused) = pool.submit(job) {
                job = refused;
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        // Both workers are inside their jobs: the queue holds one, refuses the next.
        started.wait();
        let (ran, runs) = mpsc::channel();
        assert!(pool.submit(Box::new(move || ran.send(()).unwrap())).is_ok());
        assert!(pool.submit(Box::new(|| {})).is_err());
        release.wait();
        drop(pool);
        assert!(
            runs.try_recv().is_ok(),
            "queued job ran before the pool joined"
        );
    }
}
