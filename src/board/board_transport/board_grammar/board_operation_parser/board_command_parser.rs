//! Parses board commands, including inbox and local commit ingestion.
use super::{
    BoardCommand, BoardTextPayload, board_read_parser,
    board_syntax_validation::{check_flags, fixed_words, optional_text, recipient, word},
    board_task_claim_parser::{parse_claim, parse_task},
};
use crate::board::board_protocol::ReadScope;
use crate::{
    board::{
        board_actor::HarnessLabel,
        board_ids::{BoardRef, EntryId, EventSeq, PlanRevision},
        board_protocol::BoardOp,
        board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle},
    },
    cli::Arguments,
};
use anyhow::{Context, Result, bail};

pub(super) fn parse_board(options: &Arguments, payload: &BoardTextPayload) -> Result<BoardCommand> {
    let words = &options.words;
    let verb = words.get(1).map(String::as_str).unwrap_or("inbox");
    if let Some(op) = board_read_parser::parse_read(options, payload)? {
        return Ok(BoardCommand::Op(op));
    }
    let op = match verb {
        "web" => {
            check_flags(options, &[])?;
            fixed_words(options, 2, 3, "board web [P7|P7@12|E512]")?;
            let target = words
                .get(2)
                .map(|value| value.parse::<BoardRef>())
                .transpose()?;
            if let Some(target) = &target {
                target.validate()?;
                if !matches!(
                    target,
                    BoardRef::Plan(_) | BoardRef::Revision(_) | BoardRef::Entry(_)
                ) {
                    bail!("invalid_reference: web requires a plan, revision, or entry");
                }
            }
            return Ok(BoardCommand::Web { target });
        }
        "hello" => {
            check_flags(options, &[])?;
            fixed_words(options, 3, 4, "board hello MODEL [EFFORT]")?;
            BoardOp::Hello {
                model: word(options, 2, "board hello MODEL [EFFORT]")?.into(),
                effort: words.get(3).cloned(),
            }
        }
        "inbox" => return parse_inbox(options, 2),
        "new" => {
            check_flags(options, &["body", "steward", "text"])?;
            BoardOp::New {
                title: PlanTitle::new(payload.text.clone())?,
                body: PlanText::new(payload.body.clone().unwrap_or_default())?,
                steward: options
                    .board
                    .steward
                    .as_deref()
                    .map(HarnessLabel::parse)
                    .transpose()?,
                repo_key: None,
            }
        }
        "post" => {
            check_flags(options, &["to", "supersedes", "text"])?;
            let target = word(options, 2, "board post P7[.3] KIND TEXT")?.parse::<BoardRef>()?;
            let kind = word(options, 3, "board post P7[.3] KIND TEXT")?.parse::<EntryKind>()?;
            BoardOp::Post {
                target,
                kind,
                body: EntryText::new(payload.text.clone())?,
                to: recipient(options)?,
                supersedes: options
                    .board
                    .supersedes
                    .as_deref()
                    .map(str::parse::<EntryId>)
                    .transpose()?,
            }
        }
        "task" => parse_task(options, payload)?,
        "claim" => parse_claim(options, payload)?,
        "propose" | "edit" => {
            if verb == "propose" {
                check_flags(options, &["body", "text", "supersedes"])?;
            } else {
                check_flags(options, &["body", "text"])?;
            }
            let base = word(options, 2, "board propose|edit P7@12 --body FILE SUMMARY")?
                .parse::<PlanRevision>()?;
            let body = PlanText::new(
                payload
                    .body
                    .clone()
                    .context("invalid_body: propose and edit require --body FILE")?,
            )?;
            let summary = EntryText::new(payload.text.clone())?;
            if verb == "propose" {
                BoardOp::Propose {
                    base,
                    body,
                    summary,
                    supersedes: options
                        .board
                        .supersedes
                        .as_deref()
                        .map(str::parse::<EntryId>)
                        .transpose()?,
                }
            } else {
                BoardOp::Edit {
                    base,
                    body,
                    summary,
                }
            }
        }
        "accept" | "reject" => {
            check_flags(options, &["text"])?;
            let proposal =
                word(options, 2, "board accept|reject E485 [NOTE]")?.parse::<EntryId>()?;
            if verb == "accept" {
                BoardOp::Accept {
                    proposal,
                    note: optional_text(&payload.text)?,
                }
            } else {
                BoardOp::Reject {
                    proposal,
                    reason: EntryText::new(payload.text.clone())?,
                }
            }
        }
        "review" => {
            check_flags(options, &[])?;
            fixed_words(options, 3, 4, "board review P7@12 [AGENT]")?;
            BoardOp::Review {
                base: word(options, 2, "board review P7@12 [AGENT]")?.parse::<PlanRevision>()?,
                agent: words
                    .get(3)
                    .map(|value| HarnessLabel::parse(value))
                    .transpose()?,
            }
        }
        "ingest" => {
            check_flags(options, &[])?;
            fixed_words(options, 2, 2, "board ingest")?;
            return Ok(BoardCommand::Ingest);
        }
        "link" | "unlink" => {
            check_flags(options, &[])?;
            let syntax = if verb == "link" {
                "board link OID P7.3"
            } else {
                "board unlink OID P7.3"
            };
            fixed_words(options, 4, 4, syntax)?;
            let oid = crate::identity::GitOid::parse(word(options, 2, syntax)?)?;
            let task = word(options, 3, syntax)?.parse()?;
            if verb == "link" {
                BoardOp::LinkCommit {
                    oid,
                    task,
                    resolution: None,
                }
            } else {
                BoardOp::UnlinkCommit { oid, task }
            }
        }
        value if value.parse::<u64>().is_ok() => return parse_inbox(options, 1),
        _ => bail!(concat!(
            "usage: board hello|inbox|show|done|feed|attention|history|search|web|claim|post|task|propose",
            "|review|accept|reject|edit|new|ingest|link|unlink|projects"
        )),
    };
    Ok(BoardCommand::Op(op))
}

fn parse_inbox(options: &Arguments, index: usize) -> Result<BoardCommand> {
    check_flags(options, &["wait", "all", "project"])?;
    let minimum = if options.words.len() == 1 { 1 } else { index };
    fixed_words(
        options,
        minimum,
        index + 1,
        "board [inbox] [SEQ] [--wait] [--all|--project NAME|ID]",
    )?;
    let after = options
        .words
        .get(index)
        .map(|value| value.parse::<EventSeq>())
        .transpose()?;
    Ok(BoardCommand::Op(BoardOp::Inbox {
        scope: ReadScope::All,
        after,
        limit: options.limit,
    }))
}
