//! Validates transport text and dispatches backend command parsing.
use super::{
    BoardCommand,
    board_text_transport::{BoardTextPayload, command_text},
};
use crate::cli::Arguments;
use anyhow::{Result, bail};

mod board_command_parser;
mod board_feedback_parser;
mod board_read_parser;
mod board_syntax_validation;
mod board_task_claim_parser;

use board_command_parser::parse_board;
use board_feedback_parser::parse_feedback;

/// Parse board/feedback words without reading client-owned files.
pub fn parse(options: &Arguments, body: Option<&str>) -> Result<BoardCommand> {
    let verb = options.words.first().map(String::as_str);
    options.board.validate_for_verb(verb)?;
    validate_board_surface(options)?;
    let payload = command_text(options, body)?;
    if options.board.body.is_some() && payload.body.is_none() {
        bail!("invalid_body: --body must be read by the client before parsing");
    }
    if payload.body.is_some()
        && !matches!(
            (verb, options.words.get(1).map(String::as_str)),
            (Some("board"), Some("new" | "propose" | "edit"))
                | (
                    Some("feedback"),
                    Some("blocked" | "confused" | "wrong" | "missing")
                )
        )
    {
        bail!("invalid_options: a body is not valid for this board command");
    }
    if payload.import_key.is_some()
        && !matches!(
            (verb, options.words.get(1).map(String::as_str)),
            (
                Some("feedback"),
                Some("blocked" | "confused" | "wrong" | "missing")
            )
        )
    {
        bail!("invalid_options: an import key is only valid for feedback reports");
    }
    let command = match verb {
        Some("board") => parse_board(options, &payload)?,
        Some("feedback") => parse_feedback(options, payload)?,
        _ => bail!("usage: board COMMAND | feedback KIND SUMMARY"),
    };
    if let BoardCommand::Op(op) = &command {
        op.validate()?;
    }
    Ok(command)
}

/// Reject source-search options on board command surfaces.
pub(crate) fn validate_board_surface(options: &Arguments) -> Result<()> {
    for (flag, present) in [
        ("sem", options.sem),
        ("no-sem", options.no_sem),
        ("rerank", options.rerank),
        ("no-rerank", options.no_rerank),
        ("member", options.member.is_some()),
        ("cache", options.cache.is_some()),
    ] {
        if present {
            bail!("invalid_options: --{flag} is not valid for a board command");
        }
    }
    Ok(())
}
