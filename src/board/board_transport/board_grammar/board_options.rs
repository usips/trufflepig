//! Options shared by board and feedback command transport.
#[cfg(test)]
mod tests;
use anyhow::{Result, bail};
use clap::Args;
use std::path::PathBuf;

/// Options shared by the board and feedback command families.
#[derive(Args, Debug, Clone, Default)]
pub struct BoardOptions {
    /// Read a plan or feedback body from a file, or `-` for stdin.
    #[arg(long)]
    pub body: Option<PathBuf>,
    /// Address a user or harness.
    #[arg(long)]
    pub to: Option<String>,
    /// Entry corrected by this post or replaced by this proposal.
    #[arg(long)]
    pub supersedes: Option<String>,
    /// Board-wide authority scope for a grant.
    #[arg(long = "board")]
    pub board_scope: Option<String>,
    /// Attach feedback to a plan.
    #[arg(long)]
    pub plan: Option<String>,
    /// Scope of work covered by a new claimed task.
    #[arg(long)]
    pub scope: Option<String>,
    /// Resume an idle claim for the same user, host, and harness; `E#` takes over that exact claim entry.
    #[arg(long, num_args = 0..=1, require_equals = true, value_name = "E#")]
    pub resume: Option<Option<String>>,
    /// Delegated claim target or grant duration, according to the subcommand.
    #[arg(long = "for", value_name = "TARGET|DURATION")]
    pub for_value: Option<String>,
    /// Worker task role.
    #[arg(long = "role")]
    pub worker_role: Option<String>,
    /// Worker model claim.
    #[arg(long)]
    pub model: Option<String>,
    /// Worker effort claim.
    #[arg(long)]
    pub effort: Option<String>,
    /// Worker address for a hello operation.
    #[arg(long)]
    pub worker: Option<String>,
    /// Repeated files covered by a worker.
    #[arg(long = "files", action = clap::ArgAction::Append)]
    pub files: Vec<String>,
    /// Full Git object ID reported by progress.
    #[arg(long)]
    pub head: Option<String>,
    /// Current/total progress step.
    #[arg(long)]
    pub step: Option<String>,
    /// Passed/failed test counts.
    #[arg(long)]
    pub tests: Option<String>,
    /// Repeated named gate outcomes.
    #[arg(long = "gate", action = clap::ArgAction::Append)]
    pub gates: Vec<String>,
    /// Relative ETA duration.
    #[arg(long)]
    pub eta: Option<String>,
    /// Review verdict.
    #[arg(long)]
    pub verdict: Option<String>,
    /// Plan heading covered by a new claimed task.
    #[arg(long)]
    pub section: Option<String>,
    /// List only open feedback reports.
    #[arg(long)]
    pub open: bool,
    /// Include plans from every repository in collection reads.
    #[arg(long)]
    pub all: bool,
    /// Select a registered workspace project by its name or ID.
    #[arg(long, value_name = "NAME|ID", allow_hyphen_values = true)]
    pub project: Option<String>,
    /// Resume a frozen collection after its returned cursor.
    #[arg(long)]
    pub after: Option<String>,
    /// Highest event sequence included in a frozen collection.
    #[arg(long)]
    pub through: Option<String>,
    /// Raw free text, including leading hyphens.
    #[arg(long, hide = true, require_equals = true, allow_hyphen_values = true)]
    pub board_text: Option<String>,
    /// Internal normalized text, body, and stable feedback identity.
    #[arg(
        long,
        hide = true,
        require_equals = true,
        allow_hyphen_values = true,
        conflicts_with = "board_text"
    )]
    pub board_payload: Option<String>,
    /// Model claim captured by the agent wrapper.
    #[arg(long, hide = true)]
    pub agent_model: Option<String>,
    /// Effort claim captured by the agent wrapper.
    #[arg(long, hide = true)]
    pub agent_effort: Option<String>,
    /// Bounded recent call audit captured by the agent wrapper.
    #[arg(long, hide = true)]
    pub recent_calls: Option<String>,
}

impl BoardOptions {
    /// Reject board-specific options outside their command families.
    pub fn validate_for_verb(&self, verb: Option<&str>) -> Result<()> {
        if !matches!(verb, Some("board" | "feedback")) && self.has_options() {
            bail!("invalid_options: board options require board or feedback");
        }
        if self.all && self.project.is_some() {
            bail!("invalid_options: --project and --all are mutually exclusive");
        }
        if self
            .project
            .as_ref()
            .is_some_and(|selector| selector.trim().is_empty())
        {
            bail!("invalid_options: --project requires a name or ID");
        }
        if self
            .recent_calls
            .as_ref()
            .is_some_and(|text| text.len() > 2_048)
        {
            bail!("invalid_options: --recent-calls exceeds 2048 bytes");
        }
        Ok(())
    }

    /// Preserve board values when a request is routed through the daemon.
    pub fn forward(&self, args: &mut Vec<String>) {
        for (flag, value) in [
            (
                "--body",
                self.body
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
            ("--to", self.to.clone()),
            ("--supersedes", self.supersedes.clone()),
            ("--board", self.board_scope.clone()),
            ("--plan", self.plan.clone()),
            ("--scope", self.scope.clone()),
            ("--section", self.section.clone()),
            ("--for", self.for_value.clone()),
            ("--role", self.worker_role.clone()),
            ("--model", self.model.clone()),
            ("--effort", self.effort.clone()),
            ("--worker", self.worker.clone()),
            ("--head", self.head.clone()),
            ("--step", self.step.clone()),
            ("--tests", self.tests.clone()),
            ("--eta", self.eta.clone()),
            ("--verdict", self.verdict.clone()),
            ("--after", self.after.clone()),
            ("--through", self.through.clone()),
            ("--project", self.project.clone()),
            ("--board-text", self.board_text.clone()),
            ("--board-payload", self.board_payload.clone()),
            ("--agent-model", self.agent_model.clone()),
            ("--agent-effort", self.agent_effort.clone()),
            ("--recent-calls", self.recent_calls.clone()),
        ] {
            if let Some(value) = value {
                args.push(format!("{flag}={value}"));
            }
        }
        for gate in &self.gates {
            args.push(format!("--gate={gate}"));
        }
        for file in &self.files {
            args.push(format!("--files={file}"));
        }
        if self.open {
            args.push("--open".into());
        }
        if self.all {
            args.push("--all".into());
        }
        match &self.resume {
            Some(Some(target)) => args.push(format!("--resume={target}")),
            Some(None) => args.push("--resume".into()),
            None => {}
        }
    }

    fn has_options(&self) -> bool {
        self.body.is_some()
            || self.to.is_some()
            || self.supersedes.is_some()
            || self.board_scope.is_some()
            || self.plan.is_some()
            || self.scope.is_some()
            || self.section.is_some()
            || self.after.is_some()
            || self.through.is_some()
            || self.open
            || self.all
            || self.project.is_some()
            || self.resume.is_some()
            || self.for_value.is_some()
            || self.worker_role.is_some()
            || self.model.is_some()
            || self.effort.is_some()
            || self.worker.is_some()
            || !self.files.is_empty()
            || self.head.is_some()
            || self.step.is_some()
            || self.tests.is_some()
            || !self.gates.is_empty()
            || self.eta.is_some()
            || self.verdict.is_some()
            || self.board_text.is_some()
            || self.board_payload.is_some()
            || self.agent_model.is_some()
            || self.agent_effort.is_some()
            || self.recent_calls.is_some()
    }
}
