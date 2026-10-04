//! Four query-only slots, reopening any connection discarded after panicking work.
use super::deadline_error;
use crate::board::{
    board_config::BoardConfig, board_protocol::BoardError, local_board::LocalBoard,
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

pub(crate) struct ReaderPool {
    available: Mutex<Vec<Option<LocalBoard>>>,
    returned: Condvar,
}

impl ReaderPool {
    /// The writer must finish opening and migrating the database before this call.
    pub(crate) fn new(config: &BoardConfig) -> Result<Self, BoardError> {
        let mut readers = Vec::with_capacity(4);
        for _ in 0..4 {
            readers.push(Some(LocalBoard::open_read_with_timeout(
                config,
                Duration::from_secs(5),
            )?));
        }
        Ok(Self {
            available: Mutex::new(readers),
            returned: Condvar::new(),
        })
    }

    /// Materialize owned records inside `work`; no connection reaches a network send.
    pub(crate) fn with_reader<T>(
        &self,
        config: &BoardConfig,
        expires: Instant,
        work: impl FnOnce(&mut LocalBoard) -> Result<T, BoardError>,
    ) -> Result<T, BoardError> {
        let mut available = self
            .available
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.available.clear_poison();
        let reader = loop {
            if Instant::now() >= expires {
                return Err(deadline_error());
            }
            if let Some(reader) = available.pop() {
                break reader;
            }
            let remaining = expires.saturating_duration_since(Instant::now());
            let (next, _) = self
                .returned
                .wait_timeout(available, remaining)
                .unwrap_or_else(|poison| poison.into_inner());
            self.available.clear_poison();
            available = next;
        };
        drop(available);
        let mut lease = ReaderLease { pool: self, reader };
        if lease.reader.is_none() {
            lease.reader = Some(LocalBoard::open_read_with_timeout(
                config,
                expires.saturating_duration_since(Instant::now()),
            )?);
        }
        if Instant::now() >= expires {
            return Err(deadline_error());
        }
        let reader = lease.reader.as_mut().expect("lease owns its reader");
        reader.set_busy_timeout(expires.saturating_duration_since(Instant::now()))?;
        reader.set_claim_ttl(Duration::from_secs(
            config.claim_ttl_seconds().cast_unsigned(),
        ))?;
        match catch_unwind(AssertUnwindSafe(|| work(reader))) {
            Ok(result) => result,
            Err(panic) => {
                lease.reader.take();
                drop(lease);
                resume_unwind(panic)
            }
        }
    }
}

struct ReaderLease<'a> {
    pool: &'a ReaderPool,
    reader: Option<LocalBoard>,
}

impl Drop for ReaderLease<'_> {
    fn drop(&mut self) {
        let mut available = self
            .pool
            .available
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.pool.available.clear_poison();
        available.push(self.reader.take());
        self.pool.returned.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{
        board_backend::BoardBackend,
        board_protocol::{BoardOp, BoardRequest, BoardResult},
        board_vocabulary::{PlanText, PlanTitle},
    };

    fn fixture() -> (tempfile::TempDir, BoardConfig, ReaderPool) {
        let directory = crate::board::board_test_support::scratch("web-readers-");
        let config = BoardConfig::for_database(directory.path().join("board.sqlite3"));
        LocalBoard::open(&config).unwrap();
        let readers = ReaderPool::new(&config).unwrap();
        (directory, config, readers)
    }

    fn overview() -> BoardOp {
        BoardOp::Overview {
            repo_key: None,
            after: None,
            through: None,
            limit: 200,
        }
    }

    fn lease_many(
        pool: &ReaderPool,
        config: &BoardConfig,
        count: usize,
        expires: Instant,
    ) -> Result<(), BoardError> {
        if count == 0 {
            return Ok(());
        }
        pool.with_reader(config, expires, |reader| {
            let request = BoardRequest::new(
                config.actor(Some("human"), Some("web")).unwrap(),
                overview(),
            );
            assert!(matches!(
                reader.handle(&request)?.result,
                BoardResult::Overview(_)
            ));
            lease_many(pool, config, count - 1, expires)
        })
    }

    #[test]
    fn reader_pool_returns_a_lease_after_panicking_work() {
        let (_directory, config, pool) = fixture();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pool.with_reader::<()>(&config, Instant::now() + Duration::from_secs(1), |reader| {
                let request = BoardRequest::new(
                    config.actor(Some("human"), Some("web")).unwrap(),
                    overview(),
                );
                reader.handle(&request).unwrap();
                panic!("read panicked")
            })
        }));
        assert!(result.is_err());
        assert_eq!(pool.available.lock().unwrap().len(), 4);
        assert_eq!(
            pool.available
                .lock()
                .unwrap()
                .iter()
                .filter(|slot| slot.is_none())
                .count(),
            1
        );
        lease_many(&pool, &config, 4, Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(pool.available.lock().unwrap().iter().all(Option::is_some));
    }

    #[test]
    fn reader_pool_clears_poison_and_retains_query_capacity() {
        let (_directory, config, pool) = fixture();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _available = pool.available.lock().unwrap();
            panic!("availability bookkeeping panicked")
        }));
        assert!(panic.is_err());
        assert!(pool.available.is_poisoned());
        lease_many(&pool, &config, 4, Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(!pool.available.is_poisoned());
    }

    #[test]
    fn reader_lease_wait_uses_the_request_deadline() {
        let (_directory, config, pool) = fixture();
        let started = Instant::now();
        let error = lease_many(&pool, &config, 5, started + Duration::from_millis(30)).unwrap_err();
        assert_eq!(
            error.code,
            crate::board::board_protocol::BoardErrorCode::DaemonBusy
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        lease_many(&pool, &config, 4, Instant::now() + Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn pooled_connections_reject_writes_and_remain_readable() {
        let (_directory, config, pool) = fixture();
        let actor = config.actor(Some("human"), Some("web")).unwrap();
        let write = BoardRequest::new(
            actor.clone(),
            BoardOp::New {
                title: PlanTitle::new("forbidden write").unwrap(),
                body: PlanText::new("").unwrap(),
                steward: None,
                repo_key: None,
            },
        );
        let expires = Instant::now() + Duration::from_secs(1);
        assert!(
            pool.with_reader(&config, expires, |reader| reader.handle(&write))
                .is_err()
        );
        let read = BoardRequest::new(actor, overview());
        let reply = pool
            .with_reader(&config, expires, |reader| reader.handle(&read))
            .unwrap();
        assert!(matches!(reply.result, BoardResult::Overview(page) if page.plans.is_empty()));
    }
}
