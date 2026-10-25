//! Validates operation semantics and bounded request metadata.
use super::BoardOp;
use crate::{
    board_ids::{BoardRef, MAX_BOARD_NUMBER, TaskId},
    board_protocol::{
        COAUTHOR_LIMIT, LINK_LIMIT, PROGRESS_ETA_LIMIT_SECS, TaskOrder, WORKER_FILE_LIMIT,
        WORKER_FILE_PATH_LIMIT, WORKER_REF_LIMIT, bounded_metadata, validate_claim,
    },
    board_vocabulary::{
        AuthorityRole, ENTRY_TEXT_LIMIT, EntryKind, FEEDBACK_TEXT_LIMIT, TaskColumn,
    },
};
use anyhow::{Result, bail};
use tp_core::request_context::{HarnessLabel, validate_actor_component};

impl BoardOp {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Post { target, .. } | Self::Show { target } => target.validate()?,
            Self::TaskMove { task, column, .. } => {
                task.validate()?;
                if *column == TaskColumn::Transferred {
                    bail!(coded!(
                        InvalidState,
                        "transferred is a read-only task column"
                    ));
                }
            }
            Self::TaskTransfer {
                task, destination, ..
            } => {
                task.validate()?;
                if task.plan == *destination {
                    bail!(coded!(
                        InvalidReference,
                        "task transfer requires a different destination plan"
                    ));
                }
            }
            Self::ClaimTask { task, .. }
            | Self::WorkerAdd { task, .. }
            | Self::UnlinkCommit { task, .. } => task.validate()?,
            Self::Propose { base, .. } | Self::Edit { base, .. } | Self::Review { base, .. } => {
                base.validate()?
            }
            Self::Claims {
                after: Some(after), ..
            } => after.validate()?,
            Self::Overview {
                after: Some(after),
                through,
                ..
            } => {
                after.validate()?;
                if through.is_some_and(|through| after.seq > through) {
                    bail!(coded!(InvalidReference, "overview cursor exceeds through"));
                }
            }
            Self::DoneTasks {
                before: Some(before),
                ..
            } => before.validate()?,
            _ => {}
        }
        match self {
            Self::ClaimTask {
                scope: None,
                resume,
                ..
            } if !resume.is_resuming() => {
                bail!(coded!(
                    InvalidOptions,
                    "claiming a task requires scope or resume"
                ));
            }
            Self::ClaimTask {
                delegate: Some(delegate),
                ..
            } => {
                HarnessLabel::parse(delegate.harness.as_str())?;
                validate_actor_component(&delegate.session, "session")?;
            }
            Self::Hello {
                model,
                effort,
                worker,
            } => {
                validate_claim(model, "model")?;
                if let Some(effort) = effort {
                    validate_claim(effort, "effort")?;
                }
                if let Some(worker) = worker {
                    worker.validate()?;
                }
            }
            Self::Grant {
                plan,
                roles,
                grantee: _,
                expires_in,
                note: _,
            } => {
                if roles.is_empty() {
                    bail!(coded!(InvalidOptions, "grant requires at least one role"));
                }
                let mut unique = std::collections::HashSet::with_capacity(roles.len());
                if roles.iter().any(|role| !unique.insert(*role)) {
                    bail!(coded!(InvalidOptions, "grant roles cannot repeat"));
                }
                if plan.is_none() && roles.as_slice() != [AuthorityRole::Triage] {
                    bail!(coded!(
                        InvalidOptions,
                        "board-wide grants support only triage"
                    ));
                }
                if expires_in
                    .is_some_and(|seconds| seconds == 0 || seconds > PROGRESS_ETA_LIMIT_SECS)
                {
                    bail!(coded!(
                        InvalidOptions,
                        "grant duration must be 1..=31536000 seconds"
                    ));
                }
            }
            Self::Pending { limit, .. } if !(1..=200).contains(limit) => {
                bail!(coded!(InvalidOptions, "pending limit must be 1..200"));
            }
            Self::WorkerAdd {
                handle,
                model,
                effort,
                files,
                ..
            } => {
                validate_claim(model, "model")?;
                if let Some(effort) = effort {
                    validate_claim(effort, "effort")?;
                }
                crate::board_protocol::WorkerHandle::parse(handle.as_str())?;
                if files.is_empty() || files.len() > WORKER_FILE_LIMIT {
                    bail!(coded!(
                        InvalidOptions,
                        "worker requires 1..{WORKER_FILE_LIMIT} file paths"
                    ));
                }
                let mut unique = std::collections::HashSet::with_capacity(files.len());
                for file in files {
                    if file.trim().is_empty()
                        || file.len() > WORKER_FILE_PATH_LIMIT
                        || file.chars().any(char::is_control)
                    {
                        bail!(coded!(
                            InvalidOptions,
                            "worker file paths must be nonblank, at most {WORKER_FILE_PATH_LIMIT} bytes, and contain no control characters"
                        ));
                    }
                    if !unique.insert(file) {
                        bail!(coded!(InvalidOptions, "worker file paths cannot repeat"));
                    }
                }
            }
            Self::WorkerEnd { worker, .. } => worker.validate()?,
            Self::Inbox { limit, .. } if *limit == 0 || *limit > 2000 => {
                bail!(coded!(InvalidOptions, "inbox limit must be 1..2000"))
            }
            Self::Search { query, limit, .. }
                if query.trim().is_empty()
                    || query.len() > ENTRY_TEXT_LIMIT
                    || query.contains('\0')
                    || !(1..=50).contains(limit) =>
            {
                bail!(coded!(
                    InvalidOptions,
                    "search requires a nonblank query of at most {} bytes and a limit of 1..50",
                    ENTRY_TEXT_LIMIT
                ))
            }
            Self::Feed { limit, .. } if !(1..=500).contains(limit) => {
                bail!(coded!(InvalidOptions, "feed limit must be 1..500"))
            }
            Self::Overview { limit, .. }
            | Self::Attention { limit, .. }
            | Self::History { limit, .. }
            | Self::Entries { limit, .. }
            | Self::Tasks { limit, .. }
            | Self::DoneTasks { limit, .. }
            | Self::Claims { limit, .. }
            | Self::FeedbackList { limit, .. }
                if !(1..=200).contains(limit) =>
            {
                bail!(coded!(InvalidOptions, "collection limit must be 1..200"))
            }
            Self::Entries {
                after: Some(_),
                before: Some(_),
                ..
            } => {
                bail!(coded!(
                    InvalidOptions,
                    "entries accepts at most one of after and before"
                ));
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
                order,
                before,
                after,
                ceiling,
                through,
                ..
            } => {
                if *order == TaskOrder::RecentFirst {
                    if after.is_some() || ceiling.is_some() || through.is_some() {
                        bail!(coded!(
                            InvalidOptions,
                            "recent-first tasks use before, without after, ceiling, or through"
                        ));
                    }
                    if let Some(before) = before {
                        before.validate()?;
                        if before.id.plan != *plan {
                            bail!(coded!(
                                InvalidReference,
                                "task cursor belongs to another plan"
                            ));
                        }
                    }
                    return Ok(());
                }
                if before.is_some() {
                    bail!(coded!(InvalidOptions, "ordinal tasks do not accept before"));
                }
                if let Some(after) = after {
                    after.validate()?;
                    if after.plan != *plan {
                        bail!(coded!(
                            InvalidReference,
                            "task cursor belongs to another plan"
                        ));
                    }
                }
                if let Some(ceiling) = ceiling {
                    ceiling.validate()?;
                    if ceiling.plan != *plan {
                        bail!(coded!(
                            InvalidReference,
                            "task ceiling belongs to another plan"
                        ));
                    }
                } else if after.is_some() || through.is_some() {
                    bail!(coded!(
                        InvalidOptions,
                        "task continuation requires its captured ceiling"
                    ));
                }
            }
            Self::Claims {
                plan: None,
                own_stale: false,
                ..
            } => {
                bail!(coded!(
                    InvalidOptions,
                    "claims requires a plan or own_stale"
                ));
            }
            Self::Post {
                target,
                kind,
                progress,
                verdict,
                ..
            } => {
                if !matches!(target, BoardRef::Plan(_) | BoardRef::Task(_)) {
                    bail!(coded!(InvalidReference, "post requires a plan or task"));
                }
                if !kind.is_post_kind() {
                    bail!(coded!(InvalidKind, "this entry kind cannot be posted"));
                }
                if progress.is_some() && *kind != EntryKind::Progress {
                    bail!(coded!(
                        InvalidOptions,
                        "progress evidence requires a progress entry"
                    ));
                }
                if let Some(progress) = progress {
                    progress.validate()?;
                }
                if verdict.is_some() && *kind != EntryKind::Review {
                    bail!(coded!(
                        InvalidOptions,
                        "a review verdict requires a review entry"
                    ));
                }
            }
            Self::Show {
                target: BoardRef::Commit(_),
            } => bail!(coded!(
                InvalidReference,
                "show requires a plan, task, entry, revision, or revision span"
            )),
            Self::TaskCreate { section, .. } | Self::CarveClaim { section, .. } => {
                if let Some(section) = section {
                    validate_claim(section, "section")?;
                }
            }
            Self::FeedbackClose { state, .. } if !state.is_closed() => {
                bail!(coded!(
                    InvalidState,
                    "close requires fixed, wontfix, or duplicate"
                ))
            }
            Self::Feedback {
                summary,
                body,
                metadata,
                ..
            } => {
                let length = summary.as_str().len()
                    + body.as_ref().map_or(0, |text| text.as_str().len() + 2);
                if length > FEEDBACK_TEXT_LIMIT {
                    bail!(coded!(
                        InvalidBody,
                        "feedback text exceeds {FEEDBACK_TEXT_LIMIT} bytes"
                    ));
                }
                metadata.validate()?;
            }
            Self::LinkCommits { commits } => {
                if commits.len() > 2000 {
                    bail!(coded!(InvalidOptions, "commit batch exceeds 2000 records"));
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
                    bail!(coded!(
                        InvalidOptions,
                        "repository roots exceed resource limit"
                    ));
                }
                if let Some(error) = &registration.registration_error {
                    bounded_metadata(error, 4096, "registration error")?;
                }
                validate_actor_component(&registration.host, "repository host")?;
                if !registration.common_dir.is_absolute() {
                    bail!(coded!(
                        InvalidOptions,
                        "repository common_dir must be absolute"
                    ));
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
                    bail!(coded!(
                        InvalidOptions,
                        "repository common_dir must be absolute"
                    ));
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
                    bail!(coded!(
                        InvalidOptions,
                        "repository common_dir must be absolute"
                    ));
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
fn validate_commit_metadata(commit: &crate::board_protocol::LinkedCommit) -> Result<()> {
    bounded_metadata(&commit.subject, 1024, "commit subject")?;
    bounded_metadata(&commit.author, 1024, "commit author")?;
    if commit.coauthors.len() > COAUTHOR_LIMIT
        || commit.plans.len() > LINK_LIMIT
        || commit.worker_refs.len() > WORKER_REF_LIMIT
    {
        bail!(coded!(
            InvalidOptions,
            "commit metadata exceeds collection limits"
        ));
    }
    for number in [commit.files, commit.insertions, commit.deletions] {
        if number > MAX_BOARD_NUMBER {
            bail!(coded!(
                InvalidOptions,
                "commit statistics exceed SQLite integer range"
            ));
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
    let mut worker_refs = std::collections::HashSet::with_capacity(commit.worker_refs.len());
    for worker in &commit.worker_refs {
        worker.validate()?;
        if !worker_refs.insert(worker) {
            bail!(coded!(
                InvalidOptions,
                "worker references cannot repeat on a commit"
            ));
        }
    }
    let mut claim_links = std::collections::HashSet::with_capacity(commit.claim_links.len());
    for link in &commit.claim_links {
        link.task.validate()?;
        if !claim_links.insert((link.task, link.claim)) {
            bail!(coded!(
                InvalidOptions,
                "claim links cannot repeat on a commit"
            ));
        }
        if let Some(actor) = &link.actor {
            actor.validate()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board_ids::PlanId;
    use crate::board_protocol::FeedbackMetadata;
    use crate::board_vocabulary::{EntryText, FeedbackKind};

    fn feedback_op(summary: &str, body: Option<&str>) -> BoardOp {
        BoardOp::Feedback {
            kind: FeedbackKind::Missing,
            summary: EntryText::new(summary).unwrap(),
            body: body.map(|text| EntryText::new(text).unwrap()),
            plan: None,
            metadata: FeedbackMetadata {
                version: "test".into(),
                cwd: "/workspace".into(),
                ..FeedbackMetadata::default()
            },
            import_key: None,
        }
    }

    #[test]
    fn feedback_aggregate_stays_4k_with_32k_entry_text() {
        assert_eq!(ENTRY_TEXT_LIMIT, 32768);
        assert_eq!(FEEDBACK_TEXT_LIMIT, 4096);
        assert!(EntryText::new("x".repeat(FEEDBACK_TEXT_LIMIT + 1)).is_ok());
        assert!(
            feedback_op(&"s".repeat(3000), Some(&"b".repeat(1094)))
                .validate()
                .is_ok()
        );
        assert!(
            feedback_op(&"s".repeat(3000), Some(&"b".repeat(1095)))
                .validate()
                .is_err()
        );
        assert!(
            feedback_op(&"é".repeat(1000), Some(&"é".repeat(1047)))
                .validate()
                .is_ok()
        );
        assert!(
            feedback_op(&"é".repeat(1000), Some(&"é".repeat(1048)))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn task_transfer_requires_another_plan_and_transferred_is_not_a_move_target() {
        let plan = PlanId::new(1).unwrap();
        let task = TaskId::new(plan, 1).unwrap();
        let note = EntryText::new("transfer reason").unwrap();
        let same_plan = BoardOp::TaskTransfer {
            task,
            destination: plan,
            note: note.clone(),
        };
        assert!(same_plan.validate().is_err());

        let cross_plan = BoardOp::TaskTransfer {
            task,
            destination: PlanId::new(2).unwrap(),
            note,
        };
        assert!(cross_plan.validate().is_ok());

        let derived_column_move = BoardOp::TaskMove {
            task,
            column: TaskColumn::Transferred,
            to: None,
        };
        assert!(derived_column_move.validate().is_err());
    }

    #[test]
    fn plan_lifecycle_notes_use_the_general_entry_text_cap() {
        let plan = PlanId::new(1).unwrap();
        let note = EntryText::new("n".repeat(ENTRY_TEXT_LIMIT)).unwrap();
        assert!(
            BoardOp::PlanClose {
                plan,
                note: note.clone()
            }
            .validate()
            .is_ok()
        );
        assert!(BoardOp::PlanReopen { plan, note }.validate().is_ok());
    }
}
