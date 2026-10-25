//! Validates transport text and dispatches backend command parsing.
use super::{
    BoardCommand,
    board_text_transport::{BoardTextPayload, command_text},
};
use crate::board_grammar::BoardArguments;
use anyhow::{Result, bail};

mod board_command_parser;
mod board_feedback_parser;
mod board_read_parser;
mod board_syntax_validation;
mod board_task_claim_parser;
mod board_v10_parser;

use board_command_parser::parse_board;
use board_feedback_parser::parse_feedback;

/// Parse board/feedback words without reading client-owned files.
pub fn parse(options: &BoardArguments, body: Option<&str>) -> Result<BoardCommand> {
    let verb = options.words.first().map(String::as_str);
    options.board.validate_for_verb(verb)?;
    validate_board_surface(options)?;
    let payload = command_text(options, body)?;
    if options.board.body.is_some() && payload.body.is_none() {
        bail!(coded!(
            InvalidBody,
            "--body must be read by the client before parsing"
        ));
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
        bail!(coded!(
            InvalidOptions,
            "a body is not valid for this board command"
        ));
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
        bail!(coded!(
            InvalidOptions,
            "an import key is only valid for feedback reports"
        ));
    }
    let command = match verb {
        Some("board") => parse_board(options, &payload)?,
        Some("feedback") => parse_feedback(options, payload)?,
        _ => bail!(coded!(Usage, "board COMMAND | feedback KIND SUMMARY")),
    };
    if let BoardCommand::Op(op) = &command {
        op.validate()?;
    }
    Ok(command)
}

/// Reject source-search options on board command surfaces.
pub fn validate_board_surface(options: &BoardArguments) -> Result<()> {
    if let Some(flag) = options.search_flag {
        bail!(coded!(
            InvalidOptions,
            "--{flag} is not valid for a board command"
        ));
    }
    Ok(())
}
