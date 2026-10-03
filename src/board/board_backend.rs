//! Lazy local writer, bounded inbox waiting, and edge maintenance for the router.
use super::{
    board_actor::BoardActor,
    board_config::{BoardConfig, BoardConfigCache},
    board_grammar::{self, BoardCommand},
    board_ids::EventSeq,
    board_protocol::{
        AgentClaims, BoardError, BoardOp, BoardReply, BoardRequest, BoardResult, CommitLinkResult,
        InboxWait, RepoScanTarget,
    },
    board_render::render_reply,
    commit_ingest::RepoIngestor,
    local_board::LocalBoard,
    repo_identity::RepoIdentityCache,
};
use crate::{
    cli::Arguments, daemon::deadline::QueryDeadline, diagnostics::RequestContext,
    output::OutputBudget,
};
use anyhow::{Result, bail};
use std::{
    sync::{
        Arc, Condvar, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Typed operations return domain evidence; edge rendering and git stay outside it.
pub trait BoardBackend: Send {
    fn handle(&mut self, request: &BoardRequest) -> std::result::Result<BoardReply, BoardError>;
    fn max_seq(&self) -> std::result::Result<EventSeq, BoardError>;
}

const MAX_WAITERS: usize = 6;
const MAX_INBOX_WAIT: Duration = Duration::from_secs(15);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const IMPORT_BACKOFF_MAX: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub struct BoardHost {
    inner: Arc<HostState>,
}

#[derive(Default)]
struct HostState {
    config: Mutex<BoardConfigCache>,
    backend: Mutex<Option<LocalBoard>>,
    ingestor: Mutex<RepoIngestor>,
    registrations: Mutex<RepoIdentityCache>,
    sequence: Mutex<EventSeq>,
    changed: Condvar,
    waiters: AtomicUsize,
    maintenance: Mutex<IngestClock>,
}

#[derive(Default)]
struct IngestClock {
    last_started: Option<Instant>,
    running: Option<
        JoinHandle<std::result::Result<Option<super::feedback_outbox::ImportSummary>, BoardError>>,
    >,
    running_import: bool,
    last_checked: Option<Instant>,
    import_retry_at: Option<Instant>,
    import_delay: Duration,
    reported_errors: Vec<&'static str>,
}

impl IngestClock {
    fn check_due(&mut self, now: Instant) -> bool {
        if self
            .last_checked
            .is_some_and(|checked| now.duration_since(checked) < IDLE_CHECK_INTERVAL)
        {
            return false;
        }
        self.last_checked = Some(now);
        true
    }

    fn import_ready(&self, now: Instant) -> bool {
        self.import_retry_at.is_none_or(|retry| now >= retry)
    }

    fn import_finished(&mut self, now: Instant, summary: &super::feedback_outbox::ImportSummary) {
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

    fn report_once(&mut self, code: &'static str, message: &str) {
        if !self.reported_errors.contains(&code) {
            self.reported_errors.push(code);
            eprintln!("trufflepig: board maintenance {code}: {message}");
        }
    }
}

impl BoardHost {
    /// Explicit configuration keeps tests and coordinators independent of process env.
    pub fn with_config(config: BoardConfig) -> Self {
        Self {
            inner: Arc::new(HostState {
                config: Mutex::new(BoardConfigCache::with_config(config)),
                ..HostState::default()
            }),
        }
    }

    fn config(&self) -> Result<BoardConfig> {
        let mut config = self.inner.config.lock().unwrap_or_else(|poisoned| {
            self.inner.config.clear_poison();
            poisoned.into_inner()
        });
        config.get(Instant::now()).map_err(Into::into)
    }

    pub(crate) fn database_path(&self) -> Result<std::path::PathBuf> {
        let mut config = self.inner.config.lock().unwrap_or_else(|poisoned| {
            self.inner.config.clear_poison();
            poisoned.into_inner()
        });
        let _ = config.get(Instant::now());
        config
            .database_path()
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| anyhow::anyhow!("board_unavailable: database path is unavailable"))
    }

    fn idle_config(&self) -> Option<(Option<std::result::Result<BoardConfig, BoardError>>, bool)> {
        let config = match self.inner.config.try_lock() {
            Ok(config) => config,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(poisoned)) => {
                self.inner.config.clear_poison();
                poisoned.into_inner()
            }
        };
        Some((config.snapshot(), config.needs_refresh(Instant::now())))
    }

    /// Dispatches without workspace resolution, holding the writer only for backend work.
    pub fn run(
        &self,
        options: &Arguments,
        context: &RequestContext,
        deadline: QueryDeadline,
    ) -> Result<String> {
        check_deadline(deadline)?;
        let config = self.config()?;
        let actor = config.actor(context.client.as_deref(), context.session.as_deref())?;
        let command = board_grammar::parse(options, None)?;
        let budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
        let BoardCommand::Op(mut op) = command else {
            let (targets, report) = self.ingest(
                &actor,
                None,
                deadline.remaining().saturating_sub(Duration::from_secs(2)),
            )?;
            let mut reply = BoardReply::new(
                config.db_path.display().to_string(),
                BoardResult::CommitsLinked(CommitLinkResult {
                    inserted: report.inserted,
                    unknown_plans: report.unknown_plans,
                    unknown_tasks: report.unknown_tasks,
                }),
            );
            reply.warnings = report.errors;
            let _ = targets;
            return Ok(render_reply(&reply, &budget)?.text);
        };
        let mut warnings = Vec::new();
        let register_write = !op.is_read_only() && !matches!(op, BoardOp::Inbox { .. });
        let probe = lock_before(&self.inner.registrations, deadline, "repository identity")
            .and_then(|mut cache| {
                cache.register(
                    &options.root,
                    &actor.host,
                    op.plan_id(),
                    &config.repos,
                    deadline.cap(Duration::from_secs(5)),
                )
            });
        let mut registration = match probe {
            Ok(probe) => {
                let diagnostic = if matches!(&op, BoardOp::Show { .. } | BoardOp::Review { .. }) {
                    probe.status
                } else {
                    probe.warning
                };
                if let Some(diagnostic) = diagnostic {
                    warnings.push(diagnostic);
                }
                probe.registration
            }
            Err(error) => {
                warnings.push(format!("repository registration: {error:#}"));
                None
            }
        };
        if let Some(proposed) = registration.clone() {
            if register_write {
                match self.handle_by(
                    &BoardRequest::new(
                        actor.clone(),
                        BoardOp::RegisterRepo {
                            registration: proposed,
                        },
                    ),
                    deadline,
                ) {
                    Ok(reply) => match reply.result {
                        BoardResult::Registered(effective) => registration = Some(effective),
                        _ => warnings.push(
                            "board_api_mismatch: unexpected repository registration reply".into(),
                        ),
                    },
                    Err(error)
                        if error.downcast_ref::<BoardError>().map(|error| error.code)
                            == Some(super::board_protocol::BoardErrorCode::InvalidOptions) =>
                    {
                        return Err(error);
                    }
                    Err(error) => warnings.push(format!("repository registration: {error:#}")),
                }
            } else {
                // Reads use the durable identity without modifying registration rows.
                match self.repositories(&actor, None, deadline) {
                    Ok(targets) => {
                        if let Some(target) = targets.iter().find(|target| {
                            target.registration.common_dir == proposed.common_dir
                                && target.registration.host == proposed.host
                        }) {
                            if let Some(configured) = &proposed.origin_override {
                                if configured != &target.registration.repo_key {
                                    return Err(BoardError::new(super::board_protocol::BoardErrorCode::InvalidOptions,
                                        format!("origin override {configured} conflicts with registered repository identity {}",
                                            target.registration.repo_key)).into());
                                }
                            }
                            if let Some(current) = &mut registration {
                                current.repo_key = target.registration.repo_key.clone();
                            }
                        }
                    }
                    Err(error) => warnings.push(format!("repository identity lookup: {error:#}")),
                }
            }
        }
        if let BoardOp::Feedback { metadata, .. } = &mut op {
            if let Some(registration) = &registration {
                metadata.repo_key = Some(registration.repo_key.clone());
            }
            super::enrich_feedback_cwd(
                metadata,
                &options.root,
                deadline.cap(Duration::from_secs(2)),
            );
        }
        let mut request = BoardRequest::new(actor.clone(), op);
        if options.board.agent_model.is_some() || options.board.agent_effort.is_some() {
            request.claims = Some(AgentClaims {
                model: options.board.agent_model.clone(),
                effort: options.board.agent_effort.clone(),
            });
        }
        request.validate()?;
        if let BoardOp::Review { base, agent } = &request.op {
            let scan_budget = deadline
                .remaining()
                .saturating_sub(Duration::from_secs(3))
                .min(Duration::from_secs(5));
            let started = Instant::now();
            let (targets, report) = if scan_budget.is_zero() {
                (
                    self.repositories(&actor, Some(base.plan), deadline)?,
                    super::commit_ingest::IngestReport {
                        errors: vec![
                            "review git scan skipped: no remaining scan budget".to_owned(),
                        ],
                        ..super::commit_ingest::IngestReport::default()
                    },
                )
            } else {
                self.ingest(&actor, Some(base.plan), scan_budget)?
            };
            warnings.extend(report.errors);
            warnings.extend(
                report
                    .unknown_plans
                    .iter()
                    .map(|plan| format!("unknown plan {plan}; commit trailer ignored")),
            );
            warnings.extend(
                report
                    .unknown_tasks
                    .iter()
                    .map(|task| format!("unknown task {task}; commit linked to its plan")),
            );
            let reply = self.handle_by(&request, deadline)?;
            let BoardResult::Review(evidence) = &reply.result else {
                bail!("board_api_mismatch: review backend returned an unexpected result");
            };
            let mut unlinked = Vec::new();
            if let Some(agent) = agent {
                for target in &targets {
                    let remaining = scan_budget.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        warnings.push("review git scan deadline reached".to_owned());
                        break;
                    }
                    match super::commit_ingest::find_unlinked(
                        &target.registration,
                        evidence.base.created_at,
                        agent,
                        remaining,
                    ) {
                        Ok(scan) => {
                            unlinked.extend(scan.commits);
                            if let Some(error) = scan.scan_error {
                                warnings.push(error);
                            }
                        }
                        Err(error) => warnings.push(format!("unlinked scan: {error:#}")),
                    }
                }
            }
            warnings.extend(reply.warnings.clone());
            let packet = super::review_packet::assemble_review(
                evidence,
                agent.as_ref(),
                &targets,
                unlinked,
                warnings,
            );
            return Ok(super::board_render::render_review(&packet, &budget, &reply.backend)?.text);
        }
        let mut reply = if options.wait && matches!(&request.op, BoardOp::Inbox { .. }) {
            self.wait_inbox(&request, deadline)?
        } else {
            self.handle_by(&request, deadline)?
        };
        if let Some(mut registration) = registration.filter(|_| register_write) {
            // New/proposal decisions reveal their plan only after the transaction.
            if registration.plan_id.is_none() {
                if let BoardResult::Change(change) = &reply.result {
                    registration.plan_id = change.plan;
                }
            }
            if let Err(error) = self.handle_by(
                &BoardRequest::new(actor.clone(), BoardOp::RegisterRepo { registration }),
                deadline,
            ) {
                warnings.push(format!("repository registration: {error:#}"));
            }
        }
        reply.warnings.extend(warnings);
        let rendered = render_reply(&reply, &budget)?;
        if matches!(&request.op, BoardOp::Inbox { after: None, .. }) {
            if let Some(rendered_through) = rendered.rendered_seq {
                self.handle_by(
                    &BoardRequest::new(actor, BoardOp::AcknowledgeInbox { rendered_through }),
                    deadline,
                )?;
            }
        }
        Ok(rendered.text)
    }

    fn handle_by(&self, request: &BoardRequest, deadline: QueryDeadline) -> Result<BoardReply> {
        check_deadline(deadline)?;
        request.validate()?;
        let config = self.config()?;
        let (reply, seq) = {
            let mut backend = lock_before(&self.inner.backend, deadline, "writer")?;
            if backend.is_none() {
                *backend = Some(LocalBoard::open_with_timeout(
                    &config,
                    deadline.cap(Duration::from_secs(5)),
                )?);
            }
            let backend = backend.as_mut().expect("backend just initialized");
            check_deadline(deadline)?;
            backend.set_busy_timeout(deadline.cap(Duration::from_secs(5)))?;
            backend.set_claim_ttl_seconds(config.claim_ttl_seconds());
            let reply = backend.handle(request)?;
            reply.validate()?;
            let seq = if deadline.expired() {
                None
            } else {
                let _ = backend.set_busy_timeout(deadline.cap(Duration::from_millis(100)));
                backend.max_seq().ok()
            };
            (reply, seq)
        };
        // Housekeeping after a committed mutation cannot turn its receipt into
        // a retryable failure. Polling waiters also see writes without notification.
        if let Some(seq) = seq {
            let mut sequence = recover_lock(&self.inner.sequence);
            if seq > *sequence {
                *sequence = seq;
                self.inner.changed.notify_all();
            }
        }
        Ok(reply)
    }

    fn wait_inbox(&self, request: &BoardRequest, deadline: QueryDeadline) -> Result<BoardReply> {
        let mut reply = self.handle_by(request, deadline)?;
        if inbox_has_events(&reply) {
            set_wait(&mut reply, InboxWait::Ready);
            return Ok(reply);
        }
        let Some(_permit) = WaiterPermit::acquire(&self.inner.waiters) else {
            set_wait(&mut reply, InboxWait::Busy);
            return Ok(reply);
        };
        let timeout = deadline
            .remaining()
            .saturating_sub(Duration::from_secs(2))
            .min(MAX_INBOX_WAIT);
        let expires = Instant::now() + timeout;
        let mut observed = recover_lock(&self.inner.sequence);
        loop {
            let known = match &reply.result {
                BoardResult::Inbox(inbox) => inbox.latest,
                _ => EventSeq::new(0),
            };
            if *observed > known {
                drop(observed);
                reply = self.handle_by(request, deadline)?;
                if inbox_has_events(&reply) {
                    set_wait(&mut reply, InboxWait::Ready);
                    return Ok(reply);
                }
                observed = recover_lock(&self.inner.sequence);
                continue;
            }
            let remaining = expires.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                set_wait(&mut reply, InboxWait::Timeout);
                return Ok(reply);
            }
            let (guard, _) = self
                .inner
                .changed
                .wait_timeout(observed, remaining.min(Duration::from_secs(1)))
                .unwrap_or_else(|poisoned| {
                    let (mut guard, timeout) = poisoned.into_inner();
                    *guard = EventSeq::default();
                    self.inner.sequence.clear_poison();
                    (guard, timeout)
                });
            drop(guard);
            // Poll once a second even without a notification to see direct writers.
            reply = self.handle_by(request, deadline)?;
            if inbox_has_events(&reply) {
                set_wait(&mut reply, InboxWait::Ready);
                return Ok(reply);
            }
            observed = recover_lock(&self.inner.sequence);
        }
    }

    fn repositories(
        &self,
        actor: &BoardActor,
        plan: Option<super::board_ids::PlanId>,
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

    fn ingest(
        &self,
        actor: &BoardActor,
        plan: Option<super::board_ids::PlanId>,
        timeout: Duration,
    ) -> Result<(Vec<RepoScanTarget>, super::commit_ingest::IngestReport)> {
        let deadline = QueryDeadline::after(timeout);
        let mut targets = self.repositories(actor, plan, deadline)?;
        let mut ingestor = match lock_before(&self.inner.ingestor, deadline, "ingest") {
            Ok(ingestor) => ingestor,
            Err(error) => {
                return Ok((
                    targets,
                    super::commit_ingest::IngestReport {
                        errors: vec![format!("{error:#}")],
                        ..super::commit_ingest::IngestReport::default()
                    },
                ));
            }
        };
        let report = ingestor.ingest(
            &mut HostAccess(self, deadline),
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
                *clock = IngestClock::default();
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
                            super::board_protocol::BoardErrorCode::BoardUnavailable,
                            "maintenance worker panicked",
                        ),
                    };
                    clock.report_once(error.code.as_str(), &error.message);
                    if clock.running_import {
                        clock.import_finished(
                            now,
                            &super::feedback_outbox::ImportSummary {
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
                        super::board_protocol::BoardErrorCode::BoardUnavailable,
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
    ) -> std::result::Result<Option<super::feedback_outbox::ImportSummary>, BoardError> {
        let config = self.config().map_err(BoardError::from)?;
        let actor = config
            .actor(None, Some("maintenance"))
            .map_err(BoardError::from)?;
        let summary = if import_due {
            Some(super::feedback_outbox::import_pending(
                &crate::system::spool_dir(),
                &mut HostAccess(self, QueryDeadline::after(Duration::from_secs(15))),
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

struct HostAccess<'a>(&'a BoardHost, QueryDeadline);
impl BoardBackend for HostAccess<'_> {
    fn handle(&mut self, request: &BoardRequest) -> std::result::Result<BoardReply, BoardError> {
        self.0.handle_by(request, self.1).map_err(Into::into)
    }
    fn max_seq(&self) -> std::result::Result<EventSeq, BoardError> {
        let backend =
            lock_before(&self.0.inner.backend, self.1, "writer").map_err(BoardError::from)?;
        backend
            .as_ref()
            .map_or(Ok(EventSeq::new(0)), BoardBackend::max_seq)
    }
}

struct WaiterPermit<'a>(&'a AtomicUsize);
impl<'a> WaiterPermit<'a> {
    fn acquire(waiters: &'a AtomicUsize) -> Option<Self> {
        waiters
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_WAITERS).then_some(count + 1)
            })
            .ok()?;
        Some(Self(waiters))
    }
}
impl Drop for WaiterPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn inbox_has_events(reply: &BoardReply) -> bool {
    matches!(&reply.result, BoardResult::Inbox(inbox) if !inbox.events.is_empty())
}
fn set_wait(reply: &mut BoardReply, wait: InboxWait) {
    if let BoardResult::Inbox(inbox) = &mut reply.result {
        inbox.wait = wait;
    }
}

fn check_deadline(deadline: QueryDeadline) -> Result<()> {
    if deadline.expired() {
        bail!("timed_out: board query deadline expired");
    }
    Ok(())
}

fn recover_lock<T: Default>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        let mut guard = poisoned.into_inner();
        *guard = T::default();
        mutex.clear_poison();
        guard
    })
}

fn lock_before<'a, T: Default>(
    mutex: &'a Mutex<T>,
    deadline: QueryDeadline,
    _label: &str,
) -> Result<MutexGuard<'a, T>> {
    loop {
        check_deadline(deadline)?;
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(poisoned)) => {
                let mut guard = poisoned.into_inner();
                *guard = T::default();
                mutex.clear_poison();
                return Ok(guard);
            }
            Err(TryLockError::WouldBlock) => {
                std::thread::sleep(deadline.cap(Duration::from_millis(5)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panicked_board_write_rolls_back_then_reopens_for_durable_writes() {
        let scratch = std::env::var_os("TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/home/josh/.cache/codex-tmp"));
        std::fs::create_dir_all(&scratch).unwrap();
        let directory = tempfile::Builder::new().tempdir_in(scratch).unwrap();
        let database = directory.path().join("board.sqlite3");
        let host = BoardHost::with_config(BoardConfig::for_database(&database));
        let actor = host
            .config()
            .unwrap()
            .actor(None, Some("panic-test"))
            .unwrap();
        let create = |title: &str| {
            BoardRequest::new(
                actor.clone(),
                BoardOp::New {
                    title: super::super::board_vocabulary::PlanTitle::new(title).unwrap(),
                    body: super::super::board_vocabulary::PlanText::new("").unwrap(),
                    steward: None,
                },
            )
        };
        host.handle_by(&create("Before panic"), QueryDeadline::start())
            .unwrap();
        host.inner
            .backend
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .inject_panic_after_write();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            host.handle_by(&create("Rolled back"), QueryDeadline::start())
                .unwrap();
        }));
        assert!(panic.is_err());
        assert!(host.inner.backend.is_poisoned());
        host.handle_by(&create("After recovery"), QueryDeadline::start())
            .unwrap();
        assert!(!host.inner.backend.is_poisoned());
        let durable = rusqlite::Connection::open(database).unwrap();
        let titles = durable
            .prepare("SELECT title FROM plans ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(titles, ["Before panic", "After recovery"]);
        assert_eq!(
            durable
                .query_row("SELECT COUNT(*) FROM events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    fn poison<T>(mutex: &Mutex<T>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mutex.lock().unwrap();
            panic!("injected board worker panic");
        }));
        assert!(mutex.is_poisoned());
    }

    #[test]
    fn poisoned_writer_reopens_durable_board_and_recovers_other_locks() {
        let directory = tempfile::tempdir().unwrap();
        let host = BoardHost::with_config(BoardConfig::for_database(
            directory.path().join("board.sqlite3"),
        ));
        let context = RequestContext::new(None, None);
        let create =
            crate::cli::parse(&["board".into(), "new".into(), "Survives panic".into()]).unwrap();
        host.run(&create, &context, QueryDeadline::start()).unwrap();
        poison(&host.inner.backend);
        poison(&host.inner.config);
        poison(&host.inner.sequence);
        poison(&host.inner.ingestor);
        let show = crate::cli::parse(&["board".into(), "show".into()]).unwrap();
        let reply = host.run(&show, &context, QueryDeadline::start()).unwrap();
        assert!(reply.contains("Survives panic"));
        drop(lock_before(&host.inner.ingestor, QueryDeadline::start(), "ingest").unwrap());
        assert!(!host.inner.backend.is_poisoned());
        assert!(!host.inner.config.is_poisoned());
        assert!(!host.inner.sequence.is_poisoned());
        assert!(!host.inner.ingestor.is_poisoned());
    }

    #[test]
    fn maintenance_import_backoff_caps_and_recovers_without_hot_polling() {
        let now = Instant::now();
        let mut clock = IngestClock::default();
        assert!(clock.check_due(now));
        assert!(!clock.check_due(now + Duration::from_millis(1)));
        assert!(clock.check_due(now + Duration::from_secs(2)));
        let pending = super::super::feedback_outbox::ImportSummary {
            pending: 1,
            ..Default::default()
        };
        clock.import_finished(now, &pending);
        assert!(!clock.import_ready(now + Duration::from_secs(1)));
        assert!(clock.import_ready(now + Duration::from_secs(2)));
        for _ in 0..10 {
            clock.import_finished(now, &pending);
        }
        assert!(!clock.import_ready(now + Duration::from_secs(59)));
        assert!(clock.import_ready(now + Duration::from_secs(60)));
        clock.import_finished(now, &Default::default());
        assert!(clock.import_ready(now));
        clock.import_finished(now, &pending);
        assert!(clock.import_ready(now + Duration::from_secs(2)));
    }

    #[test]
    fn configured_repository_identity_conflicts_block_reads_and_writes_before_mutation() {
        let fixture = super::super::repo_identity::tests::GitFixture::new();
        fixture.commit("portable root");
        fixture.git(&["config", "remote.origin.url", "https://example.test/repo"]);
        let database = fixture.directory.path().join("board.sqlite3");
        let mut config = BoardConfig::for_database(&database);
        let host = BoardHost::with_config(config.clone());
        let run = |words: &[&str]| {
            let mut args = vec![
                "--root".to_owned(),
                fixture.root.to_string_lossy().into_owned(),
            ];
            args.extend(words.iter().map(|word| (*word).to_owned()));
            host.run(
                &crate::cli::parse(&args).unwrap(),
                &RequestContext::new(None, None),
                QueryDeadline::start(),
            )
        };
        run(&["board", "new", "First identity"]).unwrap();
        let external = rusqlite::Connection::open(&database).unwrap();
        let existing: String = external
            .query_row("SELECT repo_key FROM repo_paths LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let conflicting = super::super::board_ids::RepoKey::parse(&"f".repeat(40)).unwrap();
        assert_ne!(existing, conflicting.as_str());
        config
            .repos
            .insert("https://example.test/repo".into(), conflicting.clone());
        *host.inner.config.lock().unwrap() = BoardConfigCache::with_config(config);
        for words in [
            vec!["board", "new", "Must not mutate"],
            vec!["board", "show"],
            vec!["board", "review", "P1@1"],
            vec!["board", "inbox", "0"],
        ] {
            let error = run(&words).unwrap_err();
            assert_eq!(
                error.downcast_ref::<BoardError>().map(|error| error.code),
                Some(super::super::board_protocol::BoardErrorCode::InvalidOptions)
            );
            assert!(error.to_string().contains(&existing));
            assert!(error.to_string().contains(conflicting.as_str()));
        }
        assert_eq!(
            external
                .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            external
                .query_row("SELECT repo_key FROM repo_paths LIMIT 1", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            existing
        );
    }

    #[test]
    fn repository_failures_warn_once_for_writes_and_remain_visible_on_reads() {
        let fixture = super::super::repo_identity::tests::GitFixture::new();
        fixture.commit("valid repository root");
        std::fs::write(
            fixture.root.join(".git/refs/heads/main"),
            "not-an-object-id\n",
        )
        .unwrap();
        let host = BoardHost::with_config(BoardConfig::for_database(
            fixture.directory.path().join("board.sqlite3"),
        ));
        let run = |words: &[&str]| {
            let mut args = vec![
                "--root".to_owned(),
                fixture.root.to_string_lossy().into_owned(),
            ];
            args.extend(words.iter().map(|word| (*word).to_owned()));
            let options = crate::cli::parse(&args).unwrap();
            host.run(
                &options,
                &RequestContext::new(None, None),
                QueryDeadline::start(),
            )
            .unwrap()
        };
        assert!(run(&["board", "new", "First write"]).contains("repository registration"));
        assert!(!run(&["board", "new", "Second write"]).contains("repository registration"));
        assert!(run(&["board", "show"]).contains("repository registration"));
        assert!(run(&["board", "review", "P1@1"]).contains("repository registration"));
    }

    #[test]
    fn configuration_ttl_refresh_changes_existing_writer_claim_policy() {
        let scratch = std::env::var_os("TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/home/josh/.cache/codex-tmp"));
        std::fs::create_dir_all(&scratch).unwrap();
        let directory = tempfile::Builder::new().tempdir_in(scratch).unwrap();
        let database = directory.path().join("board.sqlite3");
        let mut config = BoardConfig::for_database(&database);
        let host = BoardHost::with_config(config.clone());
        let actor = config.actor(None, Some("claim-owner")).unwrap();
        let create = BoardRequest::new(
            actor.clone(),
            BoardOp::New {
                title: super::super::board_vocabulary::PlanTitle::new("TTL test").unwrap(),
                body: super::super::board_vocabulary::PlanText::new("").unwrap(),
                steward: None,
            },
        );
        let reply = host.handle_by(&create, QueryDeadline::start()).unwrap();
        let BoardResult::Change(change) = reply.result else {
            panic!("expected plan");
        };
        let plan = change.plan.unwrap();
        let reply = host
            .handle_by(
                &BoardRequest::new(
                    actor.clone(),
                    BoardOp::CarveClaim {
                        plan,
                        title: super::super::board_vocabulary::PlanTitle::new("Owned task")
                            .unwrap(),
                        scope: super::super::board_vocabulary::EntryText::new("ttl policy")
                            .unwrap(),
                        section: None,
                    },
                ),
                QueryDeadline::start(),
            )
            .unwrap();
        let BoardResult::Change(change) = reply.result else {
            panic!("expected task");
        };
        let task = change.task.unwrap();
        let external = rusqlite::Connection::open(&database).unwrap();
        external
            .execute("UPDATE claims SET last_active=last_active-120", [])
            .unwrap();
        config.claim_ttl_minutes = 1;
        *host.inner.config.lock().unwrap() = BoardConfigCache::with_config(config.clone());
        let takeover = config.actor(None, Some("claim-takeover")).unwrap();
        let request = BoardRequest::new(
            takeover,
            BoardOp::ClaimTask {
                task,
                scope: Some(super::super::board_vocabulary::EntryText::new("new owner").unwrap()),
                resume: false,
            },
        );
        host.handle_by(&request, QueryDeadline::start()).unwrap();
    }

    #[test]
    fn idle_maintenance_schedules_a_blocked_loader_without_waiting() {
        use std::os::unix::ffi::OsStrExt;
        let scratch = std::env::var_os("TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/home/josh/.cache/codex-tmp"));
        std::fs::create_dir_all(&scratch).unwrap();
        let directory = tempfile::Builder::new().tempdir_in(scratch).unwrap();
        let source = directory.path().join("blocked-board.toml");
        let name = std::ffi::CString::new(source.as_os_str().as_bytes()).unwrap();
        // SAFETY: CString is NUL terminated and mkfifo only creates this test path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let host = BoardHost::default();
        *host.inner.config.lock().unwrap() = BoardConfigCache::with_source(
            source.clone(),
            BoardConfig::for_database(directory.path().join("absent.sqlite3")),
        );
        let started = Instant::now();
        host.idle();
        assert!(started.elapsed() < Duration::from_millis(100));
        let worker = host
            .inner
            .maintenance
            .lock()
            .unwrap()
            .running
            .take()
            .unwrap();
        std::fs::write(source, "claim_ttl_minutes = 1").unwrap();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn idle_maintenance_does_not_wait_for_configuration_lock() {
        let host = BoardHost::with_config(BoardConfig::for_database(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/absent-board.sqlite3"),
        ));
        let held = host.inner.config.lock().unwrap();
        let started = Instant::now();
        host.idle();
        assert!(started.elapsed() < Duration::from_millis(100));
        drop(held);
    }

    #[test]
    fn expired_board_mutation_never_creates_the_database() {
        let scratch =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/board-host-tests");
        std::fs::create_dir_all(&scratch).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("expired-")
            .tempdir_in(scratch)
            .unwrap();
        let database = directory.path().join("data/board.sqlite3");
        let host = BoardHost::with_config(BoardConfig::for_database(&database));
        let options =
            crate::cli::parse(&["board".into(), "new".into(), "Never created".into()]).unwrap();
        let error = host
            .run(
                &options,
                &RequestContext::new(None, None),
                QueryDeadline::after(Duration::ZERO),
            )
            .unwrap_err();
        assert!(crate::daemon::deadline::is_timed_out(&error));
        assert!(!database.exists());
    }

    #[test]
    fn board_waiter_capacity_releases_permits_on_drop() {
        let waiters = AtomicUsize::new(0);
        let mut admitted: Vec<_> = (0..MAX_WAITERS)
            .map(|_| WaiterPermit::acquire(&waiters).unwrap())
            .collect();
        assert!(WaiterPermit::acquire(&waiters).is_none());
        admitted.pop();
        assert!(WaiterPermit::acquire(&waiters).is_some());
        drop(admitted);
        assert_eq!(waiters.load(Ordering::Acquire), 0);
    }

    #[test]
    fn writer_lock_wait_respects_the_accepted_deadline() {
        let writer = Mutex::new(());
        let held = writer.lock().unwrap();
        let started = Instant::now();
        let result = lock_before(
            &writer,
            QueryDeadline::after(Duration::from_millis(20)),
            "writer",
        );
        assert!(crate::daemon::deadline::is_timed_out(&result.unwrap_err()));
        assert!(started.elapsed() < Duration::from_millis(150));
        drop(held);
    }
}
