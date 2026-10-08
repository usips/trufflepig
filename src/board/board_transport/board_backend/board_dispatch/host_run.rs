//! Backend request runs: CLI commands onto the board host.
mod host_done;
mod host_review;
use super::super::{
    BoardHost,
    board_wait::{inbox_has_events, waiter_transient},
    board_writer::{BoardHostBackendAccess, check_deadline, lock_before},
};
use super::scope_read_repo_key;
use crate::{
    board::{
        board_grammar::{self, BoardCommand},
        board_protocol::{
            AgentClaims, BoardError, BoardErrorCode, BoardOp, BoardReply, BoardRequest,
            BoardResult, CommitLinkResult, ReadScope,
        },
        board_render::{render_cli_reply, render_reply},
    },
    cli::Arguments,
    daemon::deadline::QueryDeadline,
    diagnostics::RequestContext,
    output::OutputBudget,
};
use anyhow::{Context, Result, bail};
use std::time::Duration;

impl BoardHost {
    pub fn select_project_scope(
        &self,
        op: &mut BoardOp,
        selector: &str,
        actor_host: &str,
        deadline: QueryDeadline,
    ) -> Result<()> {
        crate::board::board_projects::validate_scope_selection(op)?;
        let projects = self.projects(actor_host, deadline)?;
        crate::board::board_projects::select_scope(op, selector, &projects).map_err(Into::into)
    }

    /// Dispatches concrete reads and writes, holding the writer only for backend work.
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
        let probe = if (options.board.all && matches!(&op, BoardOp::Inbox { .. }))
            || matches!(
                &op,
                BoardOp::LinkCommit { .. } | BoardOp::UnlinkCommit { .. }
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
        if let Some(selector) = options.board.project.as_deref() {
            if !self.validate_done_project(&op, selector, &actor.host, deadline)? {
                if selector == "unscoped" {
                    *op.read_scope_mut()
                        .context("invalid_options: --project requires a scoped read")? =
                        ReadScope::Unscoped;
                } else {
                    self.select_project_scope(&mut op, selector, &actor.host, deadline)?;
                }
            }
        }
        scope_read_repo_key(&mut op, registration.as_ref(), options.board.all);
        let mut request = BoardRequest::new(actor.clone(), op);
        if options.board.agent_model.is_some() || options.board.agent_effort.is_some() {
            request.claims = Some(AgentClaims {
                model: options.board.agent_model.clone(),
                effort: options.board.agent_effort.clone(),
            });
        }
        request.validate()?;
        if let BoardOp::Review { base, agent } = &request.op {
            return self.run_review(&request, base, agent.as_ref(), &budget, deadline, warnings);
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
        let rendered = render_cli_reply(&reply, &budget, options.board.project.as_deref())?;
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
