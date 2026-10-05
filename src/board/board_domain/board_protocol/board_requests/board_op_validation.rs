//! Validates operation semantics and bounded request metadata.
use super::BoardOp;
use crate::board::{
    board_actor::{HarnessLabel, validate_actor_component},
    board_ids::{BoardRef, MAX_BOARD_NUMBER, TaskId},
    board_protocol::{COAUTHOR_LIMIT, LINK_LIMIT, bounded_metadata, validate_claim},
    board_vocabulary::ENTRY_TEXT_LIMIT,
};
use anyhow::{Result, bail};

impl BoardOp {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Post { target, .. } | Self::Show { target } => target.validate()?,
            Self::TaskMove { task, .. } | Self::ClaimTask { task, .. } => task.validate()?,
            Self::Propose { base, .. } | Self::Edit { base, .. } | Self::Review { base, .. } => {
                base.validate()?
            }
            Self::Claims {
                after: Some(after), ..
            } => after.validate()?,
            _ => {}
        }
        match self {
            Self::ClaimTask {
                scope: None,
                resume,
                ..
            } if !resume.is_resuming() => {
                bail!("invalid_options: claiming a task requires scope or resume");
            }
            Self::ClaimTask {
                delegate: Some(delegate),
                ..
            } => {
                HarnessLabel::parse(delegate.harness.as_str())?;
                validate_actor_component(&delegate.session, "session")?;
            }
            Self::Hello { model, effort } => {
                validate_claim(model, "model")?;
                if let Some(effort) = effort {
                    validate_claim(effort, "effort")?;
                }
            }
            Self::Inbox { limit, .. } if *limit == 0 || *limit > 2000 => {
                bail!("invalid_options: inbox limit must be 1..2000")
            }
            Self::Search { query, limit, .. }
                if query.trim().is_empty()
                    || query.len() > ENTRY_TEXT_LIMIT
                    || query.contains('\0')
                    || !(1..=50).contains(limit) =>
            {
                bail!(
                    "invalid_options: search requires a nonblank query of at most 4096 bytes and a limit of 1..50"
                )
            }
            Self::Feed { limit, .. } if !(1..=500).contains(limit) => {
                bail!("invalid_options: feed limit must be 1..500")
            }
            Self::Overview { limit, .. }
            | Self::Attention { limit, .. }
            | Self::History { limit, .. }
            | Self::Entries { limit, .. }
            | Self::Tasks { limit, .. }
            | Self::Claims { limit, .. }
            | Self::FeedbackList { limit, .. }
                if !(1..=200).contains(limit) =>
            {
                bail!("invalid_options: collection limit must be 1..200")
            }
            Self::Entries {
                after: Some(_),
                before: Some(_),
                ..
            } => {
                bail!("invalid_options: entries accepts at most one of after and before");
            }
            Self::Entries {
                user, host, task, ..
            } => {
                if let Some(user) = user {
                    validate_actor_component(user, "user")?;
                }
                if let Some(host) = host {
                    validate_actor_component(host, "host")?;
                }
                if let Some(task) = task {
                    task.validate()?;
                }
            }
            Self::Tasks {
                plan,
                after,
                ceiling,
                through,
                ..
            } => {
                if let Some(after) = after {
                    after.validate()?;
                    if after.plan != *plan {
                        bail!("invalid_reference: task cursor belongs to another plan");
                    }
                }
                if let Some(ceiling) = ceiling {
                    ceiling.validate()?;
                    if ceiling.plan != *plan {
                        bail!("invalid_reference: task ceiling belongs to another plan");
                    }
                } else if after.is_some() || through.is_some() {
                    bail!("invalid_options: task continuation requires its captured ceiling");
                }
            }
            Self::Claims {
                plan: None,
                own_stale: false,
                ..
            } => {
                bail!("invalid_options: claims requires a plan or own_stale");
            }
            Self::Post { target, kind, .. } => {
                if !matches!(target, BoardRef::Plan(_) | BoardRef::Task(_)) {
                    bail!("invalid_reference: post requires a plan or task");
                }
                if !kind.is_post_kind() {
                    bail!("invalid_kind: this entry kind cannot be posted");
                }
            }
            Self::Show {
                target: BoardRef::Commit(_),
            } => bail!(
                "invalid_reference: show requires a plan, task, entry, revision, or revision span"
            ),
            Self::TaskCreate { section, .. } | Self::CarveClaim { section, .. } => {
                if let Some(section) = section {
                    validate_claim(section, "section")?;
                }
            }
            Self::FeedbackClose { state, .. } if !state.is_closed() => {
                bail!("invalid_state: close requires fixed, wontfix, or duplicate")
            }
            Self::Feedback {
                summary,
                body,
                metadata,
                ..
            } => {
                let length = summary.as_str().len()
                    + body.as_ref().map_or(0, |text| text.as_str().len() + 2);
                if length > ENTRY_TEXT_LIMIT {
                    bail!("invalid_body: feedback text exceeds 4096 bytes");
                }
                metadata.validate()?;
            }
            Self::LinkCommits { commits } => {
                if commits.len() > 2000 {
                    bail!("invalid_options: commit batch exceeds 2000 records");
                }
                for commit in commits {
                    validate_commit_metadata(commit)?;
                }
            }
            Self::LinkCommit {
                task, resolution, ..
            } => {
                task.validate()?;
                if let Some(commit) = resolution {
                    validate_commit_metadata(commit)?;
                }
            }
            Self::RegisterRepo { registration } => {
                if registration.root_commits.len() > 4096 {
                    bail!("invalid_options: repository roots exceed resource limit");
                }
                if let Some(error) = &registration.registration_error {
                    bounded_metadata(error, 4096, "registration error")?;
                }
                validate_actor_component(&registration.host, "repository host")?;
                if !registration.common_dir.is_absolute() {
                    bail!("invalid_options: repository common_dir must be absolute");
                }
                if let Some(origin) = &registration.origin_label {
                    bounded_metadata(origin, 4096, "origin label")?;
                }
            }
            Self::ForgetRepoPath {
                host, common_dir, ..
            } => {
                validate_actor_component(host, "repository host")?;
                if !common_dir.is_absolute() {
                    bail!("invalid_options: repository common_dir must be absolute");
                }
            }
            Self::RecordScan {
                host,
                common_dir,
                error,
                ..
            } => {
                validate_actor_component(host, "repository host")?;
                if !common_dir.is_absolute() {
                    bail!("invalid_options: repository common_dir must be absolute");
                }
                if let Some(error) = error {
                    bounded_metadata(error, 4096, "scan error")?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Bounds one commit's metadata the same way on scan and manual-link paths.
fn validate_commit_metadata(commit: &crate::board::board_protocol::LinkedCommit) -> Result<()> {
    bounded_metadata(&commit.subject, 1024, "commit subject")?;
    bounded_metadata(&commit.author, 1024, "commit author")?;
    if commit.coauthors.len() > COAUTHOR_LIMIT || commit.plans.len() > LINK_LIMIT {
        bail!("invalid_options: commit metadata exceeds collection limits");
    }
    for number in [commit.files, commit.insertions, commit.deletions] {
        if number > MAX_BOARD_NUMBER {
            bail!("invalid_options: commit statistics exceed SQLite integer range");
        }
    }
    for author in &commit.coauthors {
        bounded_metadata(&author.model, 256, "co-author model")?;
        bounded_metadata(&author.email, 256, "co-author email")?;
    }
    for link in &commit.plans {
        if let Some(ordinal) = link.task_ordinal {
            TaskId::new(link.plan_id, ordinal)?;
        }
    }
    Ok(())
}
