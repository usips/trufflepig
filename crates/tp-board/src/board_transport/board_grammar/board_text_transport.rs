//! Normalized command text and body preflight for client transport.
use super::{BoardCommand, parse};
use crate::{
    board_grammar::BoardArguments,
    {
        board_protocol::BoardOp,
        board_vocabulary::{FEEDBACK_TEXT_LIMIT, FeedbackImportKey, PLAN_TEXT_LIMIT},
    },
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tp_core::coded_error::CodedContext;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BoardTextPayload {
    pub(super) text: String,
    pub(super) body: Option<String>,
    pub(super) import_key: Option<FeedbackImportKey>,
    pub(super) steer_mode: Option<String>,
}

/// Remove client paths and free positional text before router forwarding;
/// the forwarded root is canonical.
pub fn normalize_args(options: &BoardArguments, body: Option<&str>) -> Result<BoardArguments> {
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
    forwarded.root = options
        .root
        .canonicalize()
        .with_coded_context(|| coded!(InvalidRoot, "cannot open board cwd"))?;
    Ok(forwarded)
}

fn text_position(options: &BoardArguments) -> Option<usize> {
    let words = &options.words;
    if matches!(
        (
            words.first().map(String::as_str),
            words.get(1).map(String::as_str)
        ),
        (Some("board"), Some("grant"))
    ) && words.get(2).is_some_and(|target| target.starts_with('P'))
    {
        return Some(4);
    }
    if matches!(
        (
            words.first().map(String::as_str),
            words.get(1).map(String::as_str)
        ),
        (Some("board"), Some("worker"))
    ) && words.get(3).map(String::as_str) == Some("end")
    {
        return Some(6);
    }
    match (
        words.first().map(String::as_str),
        words.get(1).map(String::as_str),
    ) {
        (Some("board"), Some("new" | "search")) => Some(2),
        (
            Some("board"),
            Some("claim" | "propose" | "edit" | "accept" | "reject" | "close" | "reopen"),
        ) => Some(3),
        (Some("board"), Some("transfer")) => Some(4),
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

pub(super) fn command_text(
    options: &BoardArguments,
    body: Option<&str>,
) -> Result<BoardTextPayload> {
    let positional = text_position(options)
        .map(|start| options.words.get(start..).unwrap_or_default().join(" "))
        .unwrap_or_default();
    if let Some(encoded) = &options.board.board_payload {
        if !positional.is_empty() || options.board.board_text.is_some() {
            bail!(coded!(InvalidOptions, "text supplied twice"));
        }
        let mut payload: BoardTextPayload =
            serde_json::from_str(encoded).with_coded_context(|| {
                coded!(
                    InvalidOptions,
                    "--board-payload must be a normalized board payload"
                )
            })?;
        if body.is_some() && payload.body.is_some() {
            bail!(coded!(InvalidOptions, "body supplied twice"));
        }
        payload.body = payload.body.or_else(|| body.map(str::to_owned));
        return Ok(payload);
    }
    if let Some(raw) = &options.board.board_text {
        if !positional.is_empty() {
            bail!(coded!(
                InvalidOptions,
                "positional text conflicts with --board-text"
            ));
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
pub fn validate_before_body(options: &BoardArguments) -> Result<()> {
    let placeholder = options.board.body.as_ref().map(|_| {
        if options.words.first().map(String::as_str) == Some("feedback") {
            "x"
        } else {
            ""
        }
    });
    parse(options, placeholder).map(|_| ())
}

pub fn body_limit(options: &BoardArguments) -> usize {
    if options.words.first().map(String::as_str) == Some("feedback") {
        FEEDBACK_TEXT_LIMIT
    } else {
        PLAN_TEXT_LIMIT
    }
}
