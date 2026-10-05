//! Parses task creation, task moves, and claim resume semantics.
use super::{
    BoardTextPayload,
    board_syntax_validation::{check_flags, fixed_words, optional_text, recipient, word},
};
use crate::{
    board::{
        board_ids::{BoardRef, EntryId},
        board_protocol::{BoardOp, ClaimDelegate, ClaimResume},
        board_vocabulary::{EntryText, PlanTitle, TaskColumn},
    },
    cli::Arguments,
};
use anyhow::{Context, Result, bail};

pub(super) fn parse_task(options: &Arguments, payload: &BoardTextPayload) -> Result<BoardOp> {
    let target = word(options, 2, "board task P7 TITLE | P7.3 COLUMN")?.parse::<BoardRef>()?;
    match target {
        BoardRef::Plan(plan) => {
            check_flags(options, &["to", "text"])?;
            Ok(BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new(payload.text.clone())?,
                to: recipient(options)?,
                section: None,
            })
        }
        BoardRef::Task(task) => {
            check_flags(options, &["to"])?;
            fixed_words(
                options,
                4,
                4,
                "board task P7.3 todo|doing|review|done|blocked",
            )?;
            Ok(BoardOp::TaskMove {
                task,
                column: word(options, 3, "board task P7.3 COLUMN")?.parse::<TaskColumn>()?,
                to: recipient(options)?,
            })
        }
        _ => bail!("invalid_reference: task requires a plan or task"),
    }
}

pub(super) fn parse_claim(options: &Arguments, payload: &BoardTextPayload) -> Result<BoardOp> {
    let target = word(
        options,
        2,
        "board claim P7.3 SCOPE | P7 TITLE --scope SCOPE",
    )?
    .parse::<BoardRef>()?;
    match target {
        BoardRef::Plan(plan) => {
            check_flags(options, &["scope", "section", "text"])?;
            Ok(BoardOp::CarveClaim {
                plan,
                title: PlanTitle::new(payload.text.clone())?,
                scope: EntryText::new(
                    options
                        .board
                        .scope
                        .clone()
                        .context("invalid_options: claiming a plan requires --scope")?,
                )?,
                section: options.board.section.clone(),
            })
        }
        BoardRef::Task(task) => {
            check_flags(options, &["for", "resume", "text"])?;
            let scope = optional_text(&payload.text)?;
            let resume = match &options.board.resume {
                None => ClaimResume::No,
                Some(None) => ClaimResume::Idle,
                Some(Some(target)) => ClaimResume::Entry(target.parse::<EntryId>()?),
            };
            if scope.is_none() && !resume.is_resuming() {
                bail!("invalid_options: claiming a task requires scope or --resume");
            }
            if resume == ClaimResume::Idle {
                if let Some(scope) = &scope {
                    let token = scope.as_str().trim();
                    if token.parse::<EntryId>().is_ok() {
                        bail!(
                            "invalid_options: --resume {token} passes scope text, not a resume target; use --resume={token}"
                        );
                    }
                }
            }
            let delegate = options
                .board
                .delegate
                .as_deref()
                .map(ClaimDelegate::parse)
                .transpose()?;
            Ok(BoardOp::ClaimTask {
                task,
                scope,
                resume,
                delegate,
            })
        }
        _ => bail!("invalid_reference: claim requires a plan or task"),
    }
}
