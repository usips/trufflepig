//! Normalized command text and body preflight for client transport.
use super::{BoardCommand, parse};
use crate::{
    board::{
        board_protocol::BoardOp,
        board_vocabulary::{ENTRY_TEXT_LIMIT, FeedbackImportKey, PLAN_TEXT_LIMIT},
    },
    cli::Arguments,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BoardTextPayload {
    pub(super) text: String,
    pub(super) body: Option<String>,
    pub(super) import_key: Option<FeedbackImportKey>,
    pub(super) steer_mode: Option<String>,
}

/// Remove client paths and free positional text before router parsing.
pub fn normalize_args(
    _args: &[String],
    options: &Arguments,
    body: Option<&str>,
) -> Result<Vec<String>> {
    let command = parse(options, body)?;
    let mut payload = command_text(options, body)?;
    if let BoardCommand::Op(BoardOp::Feedback { import_key, .. }) = command {
        payload.import_key = import_key;
    }
    if options.words.first().map(String::as_str) == Some("feedback")
        && options.board.board_payload.is_none()
    {
        payload.steer_mode = std::env::var("TRUFFLEPIG_AGENT_STEER").ok();
    }
    let mut forwarded = options.clone();
    if let Some(start) = text_position(options) {
        forwarded.words.truncate(start);
    }
    forwarded.board.body = None;
    forwarded.board.board_text = None;
    forwarded.board.board_payload = if text_position(options).is_some() || payload.body.is_some() {
        Some(serde_json::to_string(&payload)?)
    } else {
        None
    };
    let root = options
        .root
        .canonicalize()
        .context("invalid_root: cannot open board cwd")?;
    Ok(crate::cli::normalized_args(&forwarded, &root))
}

fn text_position(options: &Arguments) -> Option<usize> {
    let words = &options.words;
    match (
        words.first().map(String::as_str),
        words.get(1).map(String::as_str),
    ) {
        (Some("board"), Some("new")) => Some(2),
        (Some("board"), Some("claim" | "propose" | "edit" | "accept" | "reject")) => Some(3),
        (Some("board"), Some("post")) => Some(4),
        (Some("board"), Some("task"))
            if words.get(2).is_some_and(|target| !target.contains('.')) =>
        {
            Some(3)
        }
        (Some("feedback"), Some("blocked" | "confused" | "wrong" | "missing")) => Some(2),
        (Some("feedback"), Some("close")) => Some(4),
        (Some("feedback"), Some("triage")) => Some(3),
        _ => None,
    }
}

pub(super) fn command_text(options: &Arguments, body: Option<&str>) -> Result<BoardTextPayload> {
    let positional = text_position(options)
        .map(|start| options.words.get(start..).unwrap_or_default().join(" "))
        .unwrap_or_default();
    if let Some(encoded) = &options.board.board_payload {
        if !positional.is_empty() || options.board.board_text.is_some() {
            bail!("invalid_options: text supplied twice");
        }
        let mut payload: BoardTextPayload = serde_json::from_str(encoded)
            .context("invalid_options: --board-payload must be a normalized board payload")?;
        if body.is_some() && payload.body.is_some() {
            bail!("invalid_options: body supplied twice");
        }
        payload.body = payload.body.or_else(|| body.map(str::to_owned));
        return Ok(payload);
    }
    if let Some(raw) = &options.board.board_text {
        if !positional.is_empty() {
            bail!("invalid_options: positional text conflicts with --board-text");
        }
        return Ok(BoardTextPayload {
            text: raw.clone(),
            body: body.map(str::to_owned),
            import_key: None,
            steer_mode: None,
        });
    }
    Ok(BoardTextPayload {
        text: positional,
        body: body.map(str::to_owned),
        import_key: None,
        steer_mode: None,
    })
}

/// Validates references and grammar before reading client-owned bodies.
pub fn validate_before_body(options: &Arguments) -> Result<()> {
    let placeholder = options.board.body.as_ref().map(|_| {
        if options.words.first().map(String::as_str) == Some("feedback") {
            "x"
        } else {
            ""
        }
    });
    parse(options, placeholder).map(|_| ())
}

pub fn body_limit(options: &Arguments) -> usize {
    if options.words.first().map(String::as_str) == Some("feedback") {
        ENTRY_TEXT_LIMIT
    } else {
        PLAN_TEXT_LIMIT
    }
}
