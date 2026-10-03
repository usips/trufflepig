//! Lazy local writer, bounded inbox waiting, and edge maintenance for the router.
use super::{
    board_actor::BoardActor,
    board_config::BoardConfig,
    board_grammar::{self, BoardCommand},
    board_ids::EventSeq,
    board_protocol::{
        AgentClaims, BoardError, BoardOp, BoardReply, BoardRequest, BoardResult, CommitLinkResult,
        InboxWait, RepoScanTarget,
    },
    board_render::render_reply,
    commit_ingest::RepoIngestor,
    local_board::LocalBoard,
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

#[derive(Clone, Default)]
pub struct BoardHost {
    inner: Arc<HostState>,
}

#[derive(Default)]
struct HostState {
    config: Mutex<Option<BoardConfig>>,
    backend: Mutex<Option<LocalBoard>>,
    ingestor: Mutex<RepoIngestor>,
    sequence: Mutex<EventSeq>,
    changed: Condvar,
    waiters: AtomicUsize,
    maintenance: Mutex<IngestClock>,
}

#[derive(Default)]
struct IngestClock {
    last_started: Option<Instant>,
    running: Option<JoinHandle<()>>,
}

impl BoardHost {
    /// Explicit configuration keeps tests and coordinators independent of process env.
    pub fn with_config(config: BoardConfig) -> Self {
        Self {
            inner: Arc::new(HostState {
                config: Mutex::new(Some(config)),
                ..HostState::default()
            }),
        }
    }

    fn config(&self) -> Result<BoardConfig> {
        let mut config = self
            .inner
            .config
            .lock()
            .map_err(|_| anyhow::anyhow!("board_unavailable: config mutex poisoned"))?;
        if config.is_none() {
            *config = Some(BoardConfig::load()?);
        }
        let config = config
            .as_ref()
            .expect("configuration just initialized")
            .clone();
        config.ensure_local()?;
        Ok(config)
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
                }),
            );
            reply.warnings = report.errors;
            let _ = targets;
            return Ok(render_reply(&reply, &budget)?.text);
        };
        let mut warnings = Vec::new();
        let registration = if !op.is_read() && !matches!(op, BoardOp::Inbox { .. }) {
            match super::repo_identity::register_repository(
                &options.root,
                &actor.host,
                op.plan_id(),
                deadline.cap(Duration::from_secs(5)),
            ) {
                Ok(registration) => registration,
                Err(error) => {
                    warnings.push(format!("repository registration: {error:#}"));
                    None
                }
            }
        } else {
            None
        };
        if let Some(registration) = &registration {
            if let Err(error) = self.handle_by(
                &BoardRequest::new(
                    actor.clone(),
                    BoardOp::RegisterRepo {
                        registration: registration.clone(),
                    },
                ),
                deadline,
            ) {
                warnings.push(format!("repository registration: {error:#}"));
            }
            if let BoardOp::Feedback { metadata, .. } = &mut op {
                metadata.repo_key = Some(registration.repo_key.clone());
                super::enrich_feedback_cwd(
                    metadata,
                    &options.root,
                    deadline.cap(Duration::from_secs(2)),
                );
            }
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
        if let Some(mut registration) = registration {
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
        if let (Some(seq), Ok(mut sequence)) = (seq, self.inner.sequence.lock()) {
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
        let mut observed = self
            .inner
            .sequence
            .lock()
            .map_err(|_| anyhow::anyhow!("board_unavailable: wake mutex poisoned"))?;
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
                observed = self
                    .inner
                    .sequence
                    .lock()
                    .map_err(|_| anyhow::anyhow!("board_unavailable: wake mutex poisoned"))?;
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
                .map_err(|_| anyhow::anyhow!("board_unavailable: wake mutex poisoned"))?;
            drop(guard);
            // Poll once a second even without a notification to see direct writers.
            reply = self.handle_by(request, deadline)?;
            if inbox_has_events(&reply) {
                set_wait(&mut reply, InboxWait::Ready);
                return Ok(reply);
            }
            observed = self
                .inner
                .sequence
                .lock()
                .map_err(|_| anyhow::anyhow!("board_unavailable: wake mutex poisoned"))?;
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
        let Ok(config) = self.config() else {
            return;
        };
        let pending = std::fs::read_dir(crate::system::spool_dir()).is_ok_and(|entries| {
            entries.filter_map(std::result::Result::ok).any(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "feedback")
            })
        });
        if !config.db_path.is_file() && !pending {
            return;
        }
        let Ok(mut clock) = self.inner.maintenance.lock() else {
            return;
        };
        let ingest_due = config.db_path.is_file()
            && clock
                .last_started
                .is_none_or(|started| started.elapsed() >= MAINTENANCE_INTERVAL);
        if clock
            .running
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
            || (!pending && !ingest_due)
        {
            return;
        }
        if let Some(finished) = clock.running.take() {
            let _ = finished.join();
        }
        let host = self.clone();
        // Spawn failure and panics cannot escape into the router maintenance loop.
        if let Ok(worker) = std::thread::Builder::new()
            .name("board-maintenance".to_owned())
            .spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    host.maintain(ingest_due)
                }));
            })
        {
            clock.running = Some(worker);
            if ingest_due {
                clock.last_started = Some(Instant::now());
            }
        }
    }

    fn maintain(&self, ingest_due: bool) -> Result<()> {
        let config = self.config()?;
        let actor = config.actor(None, Some("maintenance"))?;
        super::feedback_outbox::import_pending(
            &crate::system::spool_dir(),
            &mut HostAccess(self, QueryDeadline::after(Duration::from_secs(15))),
        )?;
        if ingest_due && config.db_path.is_file() {
            self.ingest(&actor, None, Duration::from_secs(15))?;
        }
        Ok(())
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

fn lock_before<'a, T>(
    mutex: &'a Mutex<T>,
    deadline: QueryDeadline,
    label: &str,
) -> Result<MutexGuard<'a, T>> {
    loop {
        check_deadline(deadline)?;
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(_)) => bail!("board_unavailable: {label} mutex poisoned"),
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
