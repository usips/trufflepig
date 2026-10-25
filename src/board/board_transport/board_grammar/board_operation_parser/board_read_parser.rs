//! Read-only collection commands and typed continuation cursors.
use crate::board::board_protocol::ReadScope;
use crate::coded_error::CodedContext;
#[cfg(test)]
mod tests;
use super::{
    BoardTextPayload,
    board_syntax_validation::{check_flags, fixed_words, word},
};
use crate::board::board_grammar::BoardArguments;
use crate::board::board_ids::{BoardRef, EventSeq, PlanId};
use crate::board::board_protocol::{
    BoardOp, BoardRequestKey, EntryCursor, OverviewCursor, TaskCursor, TaskOrder,
};
use crate::board::board_vocabulary::TaskColumn;
use anyhow::{Result, bail, ensure};

pub(super) fn parse_read(
    options: &BoardArguments,
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
                    bail!(coded!(
                        InvalidReference,
                        "show requires a plan, task, revision, revision span, or entry"
                    ));
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
                        .map(|cursor| {
                            let (seq, plan) = cursor.split_once(':').with_coded_context(|| {
                                coded!(InvalidReference, "overview cursor must be SEQ:P7")
                            })?;
                            Ok::<_, anyhow::Error>(OverviewCursor {
                                seq: seq.parse()?,
                                plan: plan.parse::<PlanId>()?,
                            })
                        })
                        .transpose()?,
                    active_only: false,
                    through: through(options)?,
                    limit: bounded_limit(options, 200)?,
                }
            }
        }
        "receipt" => {
            check_flags(options, &[])?;
            fixed_words(options, 3, 3, "board receipt KEY")?;
            let key = options.words[2]
                .parse::<BoardRequestKey>()
                .map_err(|error| anyhow::anyhow!(coded!(InvalidReference, "{error}")))?;
            BoardOp::Receipt { key }
        }
        "done" => {
            check_flags(options, &["all", "project", "after"])?;
            fixed_words(
                options,
                2,
                3,
                "board done [P7] [--project NAME|ID] [--after SEQ:P7.N] [-n N]",
            )?;
            let before = options
                .board
                .after
                .as_deref()
                .map(|cursor| {
                    let (seq, id) = cursor.split_once(':').with_coded_context(|| {
                        coded!(InvalidReference, "Done cursor must be SEQ:P7.N")
                    })?;
                    Ok::<_, anyhow::Error>(TaskCursor {
                        seq: seq.parse()?,
                        id: id.parse()?,
                    })
                })
                .transpose()?;
            let limit = bounded_limit(options, 200)?;
            if let Some(plan) = options.words.get(2) {
                ensure!(
                    !options.board.all,
                    coded!(InvalidOptions, "--all applies only to board-wide Done")
                );
                BoardOp::Tasks {
                    plan: plan.parse::<PlanId>()?,
                    column: Some(TaskColumn::Done),
                    order: TaskOrder::RecentFirst,
                    before,
                    after: None,
                    ceiling: None,
                    through: None,
                    limit,
                }
            } else {
                BoardOp::DoneTasks {
                    scope: ReadScope::All,
                    before,
                    limit,
                }
            }
        }
        "projects" => {
            check_flags(options, &[])?;
            fixed_words(options, 2, 2, "board projects")?;
            BoardOp::Projects
        }
        "pending" => {
            check_flags(options, &[])?;
            fixed_words(options, 2, 3, "board pending [SEQ]")?;
            BoardOp::Pending {
                after: options
                    .words
                    .get(2)
                    .map(|value| value.parse::<EventSeq>())
                    .transpose()?,
                limit: bounded_limit(options, 200)?,
            }
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
                    ensure!(
                        options.words.len() == 3,
                        coded!(Usage, "board feed [P7] [SEQ]")
                    );
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

pub(super) fn entry_after(options: &BoardArguments) -> Result<Option<EntryCursor>> {
    options
        .board
        .after
        .as_deref()
        .map(|value| {
            let (seq, entry) = value
                .split_once(':')
                .with_coded_context(|| coded!(InvalidReference, "entry cursor must be SEQ:E#"))?;
            Ok(EntryCursor {
                seq: seq.parse()?,
                entry: entry.parse()?,
            })
        })
        .transpose()
}

pub(super) fn through(options: &BoardArguments) -> Result<Option<EventSeq>> {
    options
        .board
        .through
        .as_deref()
        .map(str::parse::<EventSeq>)
        .transpose()
}

fn event_after(options: &BoardArguments, positional: Option<&str>) -> Result<Option<EventSeq>> {
    ensure!(
        positional.is_none() || options.board.after.is_none(),
        coded!(InvalidOptions, "event cursor supplied twice")
    );
    positional
        .or(options.board.after.as_deref())
        .map(str::parse::<EventSeq>)
        .transpose()
}

pub(super) fn bounded_limit(options: &BoardArguments, maximum: usize) -> Result<usize> {
    ensure!(
        (1..=maximum).contains(&options.limit),
        coded!(
            InvalidOptions,
            "this board collection limit must be 1..{maximum}"
        )
    );
    Ok(options.limit)
}
