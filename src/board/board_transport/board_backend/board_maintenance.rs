//! Bounded repository ingestion and background maintenance scheduling.

use super::{
    BoardHost, BoardMaintenanceClock,
    board_writer::{BoardHostBackendAccess, lock_before},
};
use crate::{
    board::{
        board_actor::BoardActor,
        board_protocol::{BoardError, BoardOp, BoardRequest, BoardResult, RepoScanTarget},
    },
    daemon::deadline::QueryDeadline,
};
use anyhow::{Result, bail};
use std::{
    sync::TryLockError,
    time::{Duration, Instant},
};

const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const IMPORT_BACKOFF_MAX: Duration = Duration::from_secs(60);

impl BoardMaintenanceClock {
    pub(super) fn check_due(&mut self, now: Instant) -> bool {
        if self
            .last_checked
            .is_some_and(|checked| now.duration_since(checked) < IDLE_CHECK_INTERVAL)
        {
            return false;
        }
        self.last_checked = Some(now);
        true
    }

    pub(super) fn import_ready(&self, now: Instant) -> bool {
        self.import_retry_at.is_none_or(|retry| now >= retry)
    }

    pub(super) fn import_finished(
        &mut self,
        now: Instant,
        summary: &crate::board::feedback_outbox::ImportSummary,
    ) {
        if summary.pending == 0 {
            self.import_retry_at = None;
            self.import_delay = Duration::ZERO;
        } else {
            self.import_delay = if self.import_delay.is_zero() {
                IDLE_CHECK_INTERVAL
            } else {
                self.import_delay.saturating_mul(2).min(IMPORT_BACKOFF_MAX)
            };
            self.import_retry_at = Some(now + self.import_delay);
        }
    }

    pub(super) fn report_once(&mut self, code: &'static str, message: &str) {
        if !self.reported_errors.contains(&code) {
            self.reported_errors.push(code);
            eprintln!("trufflepig: board maintenance {code}: {message}");
        }
    }
}

impl BoardHost {
    pub(super) fn repositories(
        &self,
        actor: &BoardActor,
        plan: Option<crate::board::board_ids::PlanId>,
        deadline: QueryDeadline,
    ) -> Result<Vec<RepoScanTarget>> {
        let reply = self.handle_by(
            &BoardRequest::new(actor.clone(), BoardOp::Repositories { plan }),
            deadline,
        )?;
        let BoardResult::Repositories(targets) = reply.result else {
            bail!("board_api_mismatch: repositories backend returned an unexpected result");
        };
        Ok(targets
            .into_iter()
            .filter(|target| target.registration.host == actor.host)
            .collect())
    }

    pub(super) fn ingest(
        &self,
        actor: &BoardActor,
        plan: Option<crate::board::board_ids::PlanId>,
        timeout: Duration,
    ) -> Result<(
        Vec<RepoScanTarget>,
        crate::board::commit_ingest::IngestReport,
    )> {
        let deadline = QueryDeadline::after(timeout);
        let mut targets = self.repositories(actor, plan, deadline)?;
        let mut ingestor = match lock_before(&self.inner.ingestor, deadline, "ingest") {
            Ok(ingestor) => ingestor,
            Err(error) => {
                return Ok((
                    targets,
                    crate::board::commit_ingest::IngestReport {
                        errors: vec![format!("{error:#}")],
                        ..crate::board::commit_ingest::IngestReport::default()
                    },
                ));
            }
        };
        let report = ingestor.ingest(
            &mut BoardHostBackendAccess(self, deadline),
            actor,
            &targets,
            deadline.remaining(),
        )?;
        for target in &mut targets {
            if report.completed.iter().any(|registration| {
                registration.repo_key == target.registration.repo_key
                    && registration.host == target.registration.host
                    && registration.common_dir == target.registration.common_dir
            }) {
                target.scan_error = None;
            }
        }
        Ok((targets, report))
    }

    /// Starts bounded maintenance without running git or importer work on daemon idle.
    pub fn idle(&self) {
        let mut clock = match self.inner.maintenance.try_lock() {
            Ok(clock) => clock,
            Err(TryLockError::WouldBlock) => return,
            Err(TryLockError::Poisoned(poisoned)) => {
                let mut clock = poisoned.into_inner();
                *clock = BoardMaintenanceClock::default();
                self.inner.maintenance.clear_poison();
                clock
            }
        };
        let now = Instant::now();
        if !clock.check_due(now) {
            return;
        }
        if clock
            .running
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return;
        }
        if let Some(finished) = clock.running.take() {
            match finished.join() {
                Ok(Ok(Some(summary))) => {
                    clock.import_finished(now, &summary);
                    if let Some(code) = summary.retry_error {
                        clock.report_once(code.as_str(), "feedback import deferred");
                    }
                }
                Ok(Ok(None)) => {}
                outcome => {
                    let error = match outcome {
                        Ok(Err(error)) => error,
                        _ => BoardError::new(
                            crate::board::board_protocol::BoardErrorCode::BoardUnavailable,
                            "maintenance worker panicked",
                        ),
                    };
                    clock.report_once(error.code.as_str(), &error.message);
                    if clock.running_import {
                        clock.import_finished(
                            now,
                            &crate::board::feedback_outbox::ImportSummary {
                                pending: 1,
                                ..Default::default()
                            },
                        );
                    }
                }
            }
        }
        let Some((snapshot, refresh_due)) = self.idle_config() else {
            return;
        };
        let config = match snapshot {
            Some(Ok(config)) => Some(config),
            Some(Err(error)) => {
                clock.report_once(error.code.as_str(), &error.message);
                None
            }
            None => None,
        };
        let pending = clock.import_ready(now)
            && std::fs::read_dir(crate::system::spool_dir()).is_ok_and(|entries| {
                entries.filter_map(std::result::Result::ok).any(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext == "feedback")
                })
            });
        let ingest_due = config
            .as_ref()
            .is_some_and(|config| config.db_path.is_file())
            && clock
                .last_started
                .is_none_or(|started| started.elapsed() >= MAINTENANCE_INTERVAL);
        if !pending && !ingest_due && !refresh_due {
            return;
        }
        let host = self.clone();
        match std::thread::Builder::new()
            .name("board-maintenance".to_owned())
            .spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    host.maintain(ingest_due, pending)
                }))
                .unwrap_or_else(|_| {
                    Err(BoardError::new(
                        crate::board::board_protocol::BoardErrorCode::BoardUnavailable,
                        "maintenance worker panicked",
                    ))
                })
            }) {
            Ok(worker) => {
                clock.running = Some(worker);
                clock.running_import = pending;
                if ingest_due {
                    clock.last_started = Some(now);
                }
            }
            Err(error) => clock.report_once("board_unavailable", &format!("spawn: {error}")),
        }
    }

    fn maintain(
        &self,
        ingest_due: bool,
        import_due: bool,
    ) -> std::result::Result<Option<crate::board::feedback_outbox::ImportSummary>, BoardError> {
        let config = self.config().map_err(BoardError::from)?;
        let actor = config
            .actor(None, Some("maintenance"))
            .map_err(BoardError::from)?;
        let summary = if import_due {
            Some(crate::board::feedback_outbox::import_pending(
                &crate::system::spool_dir(),
                &mut BoardHostBackendAccess(self, QueryDeadline::after(Duration::from_secs(15))),
            )?)
        } else {
            None
        };
        if ingest_due && config.db_path.is_file() {
            self.ingest(&actor, None, Duration::from_secs(15))
                .map_err(BoardError::from)?;
        }
        Ok(summary)
    }
}
