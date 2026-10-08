//! Read-only collection commands and typed continuation cursors.
use crate::board::board_protocol::ReadScope;
#[cfg(test)]
mod tests;
use super::{
    BoardTextPayload,
    board_syntax_validation::{check_flags, fixed_words, word},
};
use crate::board::board_ids::{BoardRef, EventSeq, PlanId};
use crate::board::board_protocol::{BoardOp, EntryCursor};
use crate::cli::Arguments;
use anyhow::{Context, Result, bail, ensure};

pub(super) fn parse_read(
    options: &Arguments,
    payload: &BoardTextPayload,
) -> Result<Option<BoardOp>> {
    let verb = options.words.get(1).map(String::as_str).unwrap_or("inbox");
    let op = match verb {
        "show" => {
            fixed_words(options, 2, 3, "board show [P7|P7.3|P7@12|E512]")?;
            if let Some(target) = options.words.get(2) {
                check_flags(options, &[])?;
                let target = target.parse::<BoardRef>()?;
                if matches!(target, BoardRef::Commit(_)) {
                    bail!(
                        "invalid_reference: show requires a plan, task, revision, revision span, or entry"
                    );
                }
                BoardOp::Show { target }
            } else {
                check_flags(options, &["all", "project", "after", "through"])?;
                BoardOp::Overview {
                    scope: ReadScope::All,
                    after: options
                        .board
                        .after
                        .as_deref()
                        .map(str::parse::<PlanId>)
                        .transpose()?,
                    through: through(options)?,
                    limit: bounded_limit(options, 200)?,
                }
            }
        }
        "projects" => {
            check_flags(options, &[])?;
            fixed_words(options, 2, 2, "board projects")?;
            BoardOp::Projects
        }
        "feed" => {
            check_flags(options, &["after", "through"])?;
            fixed_words(
                options,
                2,
                4,
                "board feed [P7] [SEQ] [--after SEQ] [--through SEQ]",
            )?;
            let (plan, positional) = match options.words.get(2) {
                Some(value) if value.starts_with('P') => (
                    Some(value.parse::<PlanId>()?),
                    options.words.get(3).map(String::as_str),
                ),
                Some(value) => {
                    ensure!(options.words.len() == 3, "usage: board feed [P7] [SEQ]");
                    (None, Some(value.as_str()))
                }
                None => (None, None),
            };
            BoardOp::Feed {
                scope: ReadScope::All,
                plan,
                after: event_after(options, positional)?,
                through: through(options)?,
                limit: bounded_limit(options, 500)?,
            }
        }
        "attention" => {
            check_flags(options, &["all", "project", "after", "through"])?;
            fixed_words(
                options,
                2,
                2,
                "board attention [--all|--project NAME|ID] [--after SEQ:E#] [--through SEQ]",
            )?;
            BoardOp::Attention {
                scope: ReadScope::All,
                after: entry_after(options)?,
                through: through(options)?,
                limit: bounded_limit(options, 200)?,
            }
        }
        "history" => {
            check_flags(options, &["after", "through"])?;
            fixed_words(
                options,
                3,
                4,
                "board history P7 [SEQ] [--after SEQ] [--through SEQ]",
            )?;
            BoardOp::History {
                plan: word(options, 2, "board history P7 [SEQ]")?.parse::<PlanId>()?,
                after: event_after(options, options.words.get(3).map(String::as_str))?,
                through: through(options)?,
                limit: bounded_limit(options, 200)?,
            }
        }
        "search" => {
            check_flags(options, &["plan", "text"])?;
            BoardOp::Search {
                query: payload.text.clone(),
                plan: options
                    .board
                    .plan
                    .as_deref()
                    .map(str::parse::<PlanId>)
                    .transpose()?,
                limit: bounded_limit(options, 50)?,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(op))
}

pub(super) fn entry_after(options: &Arguments) -> Result<Option<EntryCursor>> {
    options
        .board
        .after
        .as_deref()
        .map(|value| {
            let (seq, entry) = value
                .split_once(':')
                .context("invalid_reference: entry cursor must be SEQ:E#")?;
            Ok(EntryCursor {
                seq: seq.parse()?,
                entry: entry.parse()?,
            })
        })
        .transpose()
}

pub(super) fn through(options: &Arguments) -> Result<Option<EventSeq>> {
    options
        .board
        .through
        .as_deref()
        .map(str::parse::<EventSeq>)
        .transpose()
}

fn event_after(options: &Arguments, positional: Option<&str>) -> Result<Option<EventSeq>> {
    ensure!(
        positional.is_none() || options.board.after.is_none(),
        "invalid_options: event cursor supplied twice"
    );
    positional
        .or(options.board.after.as_deref())
        .map(str::parse::<EventSeq>)
        .transpose()
}

pub(super) fn bounded_limit(options: &Arguments, maximum: usize) -> Result<usize> {
    ensure!(
        (1..=maximum).contains(&options.limit),
        "invalid_options: this board collection limit must be 1..{maximum}"
    );
    Ok(options.limit)
}
