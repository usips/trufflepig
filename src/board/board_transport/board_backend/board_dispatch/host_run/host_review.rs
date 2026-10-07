//! Review scans and packet rendering within the request deadline.

use super::*;
use crate::board::{
    board_actor::HarnessLabel, board_ids::PlanRevision, commit_ingest::IngestReport,
};
use std::time::Instant;

impl BoardHost {
    pub(super) fn run_review(
        &self,
        request: &BoardRequest,
        base: &PlanRevision,
        agent: Option<&HarnessLabel>,
        budget: &OutputBudget,
        deadline: QueryDeadline,
        mut warnings: Vec<String>,
    ) -> Result<String> {
        let scan_budget = deadline
            .remaining()
            .saturating_sub(Duration::from_secs(3))
            .min(Duration::from_secs(5));
        let started = Instant::now();
        let (targets, report) = if scan_budget.is_zero() {
            (
                self.repositories(&request.actor, Some(base.plan), deadline)?,
                IngestReport {
                    errors: vec!["review git scan skipped: no remaining scan budget".to_owned()],
                    ..Default::default()
                },
            )
        } else {
            match self.ingest(&request.actor, Some(base.plan), scan_budget) {
                Ok(result) => result,
                Err(error) => (
                    self.repositories(&request.actor, Some(base.plan), deadline)?,
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
        let reply = self.handle_by(request, deadline)?;
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
                    Ok(mut scan) => {
                        unlinked.extend(scan.commits);
                        if let Err(error) = crate::board::commit_ingest::suppress_linked_warnings(
                            &BoardHostBackendAccess(self, deadline),
                            &target.registration.repo_key,
                            &mut scan.warnings,
                        ) {
                            warnings.push(format!("linked commit lookup: {error:#}"));
                        }
                        warnings.extend(scan.warnings);
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
            evidence, agent, &targets, unlinked, warnings,
        );
        Ok(crate::board::board_render::render_review(&packet, budget, &reply.backend)?.text)
    }
}
