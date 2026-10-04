//! Shared flag, word-count, recipient, and optional-text validation.
use crate::{
    board::{board_actor::BoardRecipient, board_vocabulary::EntryText},
    cli::Arguments,
};
use anyhow::{Context, Result, bail};

pub(super) fn check_flags(options: &Arguments, allowed: &[&str]) -> Result<()> {
    let board = &options.board;
    for (name, present) in [
        ("body", board.body.is_some()),
        ("to", board.to.is_some()),
        ("supersedes", board.supersedes.is_some()),
        ("steward", board.steward.is_some()),
        ("plan", board.plan.is_some()),
        ("scope", board.scope.is_some()),
        ("section", board.section.is_some()),
        ("after", board.after.is_some()),
        ("through", board.through.is_some()),
        ("open", board.open),
        ("all", board.all),
        ("resume", board.resume.is_some()),
        ("for", board.delegate.is_some()),
        (
            "text",
            board.board_text.is_some() || board.board_payload.is_some(),
        ),
        ("recent-calls", board.recent_calls.is_some()),
        ("wait", options.wait),
    ] {
        if present && !allowed.contains(&name) {
            let flag = if name == "text" { "board-text" } else { name };
            bail!("invalid_options: --{flag} is not valid for this board command");
        }
    }
    Ok(())
}

pub(super) fn recipient(options: &Arguments) -> Result<Option<BoardRecipient>> {
    options
        .board
        .to
        .as_deref()
        .map(BoardRecipient::parse)
        .transpose()
}

pub(super) fn optional_text(value: &str) -> Result<Option<EntryText>> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        EntryText::new(value.to_owned()).map(Some)
    }
}

pub(super) fn word<'a>(options: &'a Arguments, index: usize, usage: &str) -> Result<&'a str> {
    options
        .words
        .get(index)
        .map(String::as_str)
        .with_context(|| format!("usage: {usage}"))
}

pub(super) fn fixed_words(
    options: &Arguments,
    minimum: usize,
    maximum: usize,
    usage: &str,
) -> Result<()> {
    if !(minimum..=maximum).contains(&options.words.len()) {
        bail!("usage: {usage}");
    }
    Ok(())
}
