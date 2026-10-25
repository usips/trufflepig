//! Builds board requests from the global command line. Board grammar sees only
//! `BoardArguments`; the CLI owns parsing, defaults, and router argv.
use super::{Arguments, normalized_args};
use anyhow::Result;
use tp_board::board_grammar::BoardArguments;

impl Arguments {
    /// The board view of these arguments, naming the first source-search flag.
    pub(crate) fn board_arguments(&self) -> BoardArguments {
        let search_flag = [
            ("sem", self.sem),
            ("no-sem", self.no_sem),
            ("rerank", self.rerank),
            ("no-rerank", self.no_rerank),
            ("member", self.member.is_some()),
            ("cache", self.cache.is_some()),
            ("no-cache", self.no_cache),
        ]
        .into_iter()
        .find_map(|(flag, present)| present.then_some(flag));
        BoardArguments {
            words: self.words.clone(),
            board: self.board.clone(),
            root: self.root.clone(),
            budget: self.budget,
            limit: self.limit,
            format: self.output_format(),
            wait: self.wait,
            no_daemon: self.no_daemon,
            search_flag,
        }
    }
}

/// Reads the client-owned body once and returns the router argv that replays it.
pub(super) fn board_forwarded_args(options: &Arguments) -> Result<Vec<String>> {
    let forwarded = tp_board::prepare_client(&options.board_arguments())?;
    Ok(replay_args(options, forwarded))
}

/// Router argv carrying the forwarded board words, flags, and canonical root.
fn replay_args(options: &Arguments, forwarded: BoardArguments) -> Vec<String> {
    let mut replay = options.clone();
    replay.words = forwarded.words;
    replay.board = forwarded.board;
    normalized_args(&replay, &forwarded.root)
}

/// Router argv for `options` with `body` as the already-read client body.
#[cfg(test)]
pub(super) fn normalize_args(options: &Arguments, body: Option<&str>) -> Result<Vec<String>> {
    let forwarded = tp_board::board_grammar::normalize_args(&options.board_arguments(), body)?;
    Ok(replay_args(options, forwarded))
}

/// Board grammar applied to the global arguments.
#[cfg(test)]
pub(super) fn board_parse(
    options: &Arguments,
    body: Option<&str>,
) -> Result<tp_board::board_grammar::BoardCommand> {
    tp_board::board_grammar::parse(&options.board_arguments(), body)
}
