//! Options shared by board and feedback command transport.
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
    /// Harness entrusted with accepting plan proposals.
    #[arg(long)]
    pub steward: Option<String>,
    /// Attach feedback to a plan.
    #[arg(long)]
    pub plan: Option<String>,
    /// Scope of work covered by a new claimed task.
    #[arg(long)]
    pub scope: Option<String>,
    /// Resume a prior session's claim for the same user, host, and harness.
    #[arg(long)]
    pub resume: bool,
    /// Plan heading covered by a new claimed task.
    #[arg(long)]
    pub section: Option<String>,
    /// List only open feedback reports.
    #[arg(long)]
    pub open: bool,
    /// Include plans from every repository in inbox and attention reads.
    #[arg(long)]
    pub all: bool,
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
            ("--steward", self.steward.clone()),
            ("--plan", self.plan.clone()),
            ("--scope", self.scope.clone()),
            ("--section", self.section.clone()),
            ("--after", self.after.clone()),
            ("--through", self.through.clone()),
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
        if self.open {
            args.push("--open".into());
        }
        if self.all {
            args.push("--all".into());
        }
        if self.resume {
            args.push("--resume".into());
        }
    }

    fn has_options(&self) -> bool {
        self.body.is_some()
            || self.to.is_some()
            || self.supersedes.is_some()
            || self.steward.is_some()
            || self.plan.is_some()
            || self.scope.is_some()
            || self.section.is_some()
            || self.after.is_some()
            || self.through.is_some()
            || self.open
            || self.all
            || self.resume
            || self.board_text.is_some()
            || self.board_payload.is_some()
            || self.agent_model.is_some()
            || self.agent_effort.is_some()
            || self.recent_calls.is_some()
    }
}
