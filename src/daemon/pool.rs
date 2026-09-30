//! Fixed worker threads draining a bounded request queue: a full queue refuses
//! work instead of letting accepted connections wait unread.
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

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
}

impl RequestPool {
    pub fn new(name: &str, size: PoolSize) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<PooledJob>(size.queue);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(size.workers);
        for index in 0..size.workers.max(1) {
            let receiver = Arc::clone(&receiver);
            workers.push(
                std::thread::Builder::new()
                    .name(format!("{name}-{index}"))
                    .spawn(move || {
                        loop {
                            // The guard drops at the end of this statement, so
                            // jobs run without holding the queue lock.
                            let job = receiver.lock().map(|queue| queue.recv());
                            match job {
                                Ok(Ok(job)) => job(),
                                _ => return,
                            }
                        }
                    })?,
            );
        }
        Ok(Self {
            sender: Some(sender),
            workers,
        })
    }

    /// Queues `job`, handing it back when every worker is busy and the queue is full.
    pub fn submit(&self, job: PooledJob) -> Result<(), PooledJob> {
        let sender = self
            .sender
            .as_ref()
            .expect("pool accepts work until dropped");
        match sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job)) => Err(job),
        }
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
