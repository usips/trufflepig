//! Fixed worker threads draining a bounded request queue: a full queue refuses
//! work instead of letting accepted connections wait unread. A panicking job
//! never takes its worker down.
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// Threads sharing one bounded queue. Shutdown rejects queued work after the
/// drain window and detaches any worker whose active request has not finished.
pub(crate) struct RequestPool {
    sender: Mutex<Option<mpsc::SyncSender<PooledJob>>>,
    receiver: Arc<Mutex<mpsc::Receiver<PooledJob>>>,
    workers: Vec<JoinHandle<()>>,
    /// Jobs queued or running.
    pending: Arc<AtomicUsize>,
    shutting_down: Arc<AtomicBool>,
}

impl RequestPool {
    pub fn new(name: &str, size: PoolSize) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<PooledJob>(size.queue);
        let receiver = Arc::new(Mutex::new(receiver));
        let pending = Arc::new(AtomicUsize::new(0));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::with_capacity(size.workers);
        for index in 0..size.workers.max(1) {
            let (receiver, pending, shutting_down) = (
                Arc::clone(&receiver),
                Arc::clone(&pending),
                Arc::clone(&shutting_down),
            );
            workers.push(
                std::thread::Builder::new()
                    .name(format!("{name}-{index}"))
                    .spawn(move || {
                        loop {
                            if shutting_down.load(Ordering::Acquire) {
                                return;
                            }
                            // The guard drops at the end of this statement, so
                            // jobs run without holding the queue lock.
                            let job = receiver.lock().map(|queue| queue.recv());
                            let Ok(Ok(job)) = job else {
                                return;
                            };
                            if shutting_down.load(Ordering::Acquire) {
                                drop(job);
                                pending.fetch_sub(1, Ordering::AcqRel);
                                return;
                            }
                            // Jobs answer their own panics; this keeps the worker.
                            let _ = catch_unwind(AssertUnwindSafe(job));
                            pending.fetch_sub(1, Ordering::AcqRel);
                        }
                    })?,
            );
        }
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            receiver,
            workers,
            pending,
            shutting_down,
        })
    }

    /// Queues `job`, handing it back when every worker is busy and the queue is full.
    pub fn submit(&self, job: PooledJob) -> Result<(), PooledJob> {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(job);
        }
        let Some(sender) = sender.as_ref() else {
            return Err(job);
        };
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

    /// Refuses new work and lets idle workers exit before the process shuts down.
    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        let mut sender = self
            .sender
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        sender.take();
        drop(sender);
        let receiver = self
            .receiver
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while let Ok(job) = receiver.try_recv() {
            drop(job);
            self.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for RequestPool {
    fn drop(&mut self) {
        self.shutdown();
        for worker in self.workers.drain(..) {
            // A request that outlives the daemon drain must not hold shutdown
            // open. Its worker is detached and the daemon process exits next.
            if worker.is_finished() {
                let _ = worker.join();
            }
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
        assert!(pool.drain(Duration::from_secs(1)));
        drop(pool);
        assert!(
            runs.try_recv().is_ok(),
            "queued job ran before the pool joined"
        );
    }

    #[test]
    fn dropping_pool_does_not_join_a_blocked_request() {
        let pool = RequestPool::new(
            "blocked-pool",
            PoolSize {
                workers: 1,
                queue: 1,
            },
        )
        .unwrap();
        let (started, started_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (finished, finished_rx) = mpsc::channel();
        assert!(
            pool.submit(Box::new(move || {
                let _ = started.send(());
                let _ = release_rx.recv();
                let _ = finished.send(());
            }))
            .is_ok()
        );
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (queued_ran, queued_ran_rx) = mpsc::channel();
        let (queued_dropped, queued_dropped_rx) = mpsc::channel();
        struct DropSignal(mpsc::Sender<()>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let signal = DropSignal(queued_dropped);
        assert!(
            pool.submit(Box::new(move || {
                let _signal = signal;
                let _ = queued_ran.send(());
            }))
            .is_ok()
        );

        let (dropped, dropped_rx) = mpsc::channel();
        let dropping = std::thread::spawn(move || {
            drop(pool);
            let _ = dropped.send(());
        });
        dropped_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("pool drop joined a request that had not finished");
        queued_dropped_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("pool shutdown retained a queued request");
        assert!(queued_ran_rx.try_recv().is_err());

        release.send(()).unwrap();
        finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        dropping.join().unwrap();
    }
}
