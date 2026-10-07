//! Backend request runs: CLI commands onto the board host.
use super::super::{
    BoardHost,
    board_wait::{inbox_has_events, waiter_transient},
    board_writer::{check_deadline, lock_before},
};
use super::scope_read_repo_key;
use crate::{
    board::{
        board_grammar::{self, BoardCommand},
        board_protocol::{
            AgentClaims, BoardError, BoardErrorCode, BoardOp, BoardReply, BoardRequest,
            BoardResult, CommitLinkResult,
        },
        board_render::render_reply,
        commit_ingest::IngestReport,
    },
    cli::Arguments,
    daemon::deadline::QueryDeadline,
    diagnostics::RequestContext,
    output::OutputBudget,
};
use anyhow::{Result, bail};
use std::time::{Duration, Instant};

impl BoardHost {
    /// Dispatches without workspace resolution, holding the writer only for backend work.
    pub fn run(
        &self,
        options: &Arguments,
        context: &RequestContext,
        deadline: QueryDeadline,
    ) -> Result<String> {
        check_deadline(deadline)?;
        let command = board_grammar::parse(options, None)?;
        if matches!(&command, BoardCommand::Web { .. }) {
            bail!("invalid_options: board web must run on the client");
        }
        let config = self.config()?;
        let actor = config.actor(context.client.as_deref(), context.session.as_deref())?;
        let budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
        let BoardCommand::Op(mut op) = command else {
            let (targets, report) = self.ingest(
                &actor,
                None,
                deadline.remaining().saturating_sub(Duration::from_secs(2)),
            )?;
            let mut reply = BoardReply::new(
                format!("local:{}", config.db_path.display()),
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
        let register_write = op.registers_workspace();
        let probe = if matches!(
            &op,
            BoardOp::Inbox { all: true, .. }
                | BoardOp::LinkCommit { .. }
                | BoardOp::UnlinkCommit { .. }
        ) {
            Ok(crate::board::repo_identity::RegistrationProbe {
                registration: None,
                warning: None,
                status: None,
            })
        } else {
            lock_before(&self.inner.registrations, deadline, "repository identity").and_then(
                |mut cache| {
                    cache.register(
                        &options.root,
                        &actor.host,
                        op.plan_id(),
                        &config.repos,
                        deadline.cap(Duration::from_secs(5)),
                    )
                },
            )
        };
        let mut registration = match probe {
            Ok(probe) => {
                let diagnostic = if matches!(
                    &op,
                    BoardOp::Show { .. } | BoardOp::Overview { .. } | BoardOp::Review { .. }
                ) {
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
                            == Some(BoardErrorCode::InvalidOptions) =>
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
                                    return Err(BoardError::new(
                                        crate::board::board_protocol::BoardErrorCode::InvalidOptions,
                                        format!(
                                            concat!(
                                                "origin override {configured} conflicts with ",
                                                "registered repository identity {}"
                                            ),
                                            target.registration.repo_key,
                                            configured = configured
                                        ),
                                    )
                                    .into());
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
            crate::board::enrich_feedback_cwd(
                metadata,
                &options.root,
                deadline.cap(Duration::from_secs(2)),
            );
        }
        scope_read_repo_key(&mut op, registration.as_ref());
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
                    IngestReport {
                        errors: vec![
                            "review git scan skipped: no remaining scan budget".to_owned(),
                        ],
                        ..Default::default()
                    },
                )
            } else {
                match self.ingest(&actor, Some(base.plan), scan_budget) {
                    Ok(result) => result,
                    Err(error) => (
                        self.repositories(&actor, Some(base.plan), deadline)?,
                        IngestReport {
                            errors: vec![format!("review git scan unavailable: {error:#}")],
                            ..Default::default()
                        },
                    ),
                }
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
                    match crate::board::commit_ingest::find_unlinked(
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
            let packet = crate::board::review_packet::assemble_review(
                evidence,
                agent.as_ref(),
                &targets,
                unlinked,
                warnings,
            );
            return Ok(crate::board::board_render::render_review(
                &packet,
                &budget,
                &reply.backend,
            )?
            .text);
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
            if let Some(rendered_through) = rendered.acknowledge_seq {
                if let Err(error) = self.handle_by(
                    &BoardRequest::new(actor, BoardOp::AcknowledgeInbox { rendered_through }),
                    deadline,
                ) {
                    if !(options.wait && !inbox_has_events(&reply) && waiter_transient(&error)) {
                        return Err(error);
                    }
                }
            }
        }
        Ok(rendered.text)
    }
}
