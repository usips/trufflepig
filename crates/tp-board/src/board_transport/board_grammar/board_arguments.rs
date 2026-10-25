//! The request a board or feedback command reads: its words, board flags, and
//! the global options board grammar and dispatch honor. The CLI builds it from
//! its own arguments; board never parses the global command line.
use super::BoardOptions;
use std::path::PathBuf;
use tp_core::output::OutputFormat;

/// Board request fields, independent of the CLI's global argument type.
#[derive(Clone, Debug)]
pub struct BoardArguments {
    pub words: Vec<String>,
    pub board: BoardOptions,
    pub root: PathBuf,
    pub budget: usize,
    pub limit: usize,
    pub format: OutputFormat,
    pub wait: bool,
    pub no_daemon: bool,
    /// First source-search flag present on the command line; board surfaces reject it.
    pub search_flag: Option<&'static str>,
}

impl BoardArguments {
    /// The page format selected by `--format`.
    pub fn output_format(&self) -> OutputFormat {
        self.format
    }

    /// Page size a board or feedback read uses when `--limit` is absent.
    pub fn default_limit(words: &[String]) -> Option<usize> {
        match (
            words.first().map(String::as_str),
            words.get(1).map(String::as_str),
        ) {
            (Some("board"), Some("feed" | "attention" | "history" | "done"))
            | (Some("feedback"), Some("ls")) => Some(200),
            (Some("board"), Some("show")) if words.len() == 2 => Some(200),
            (Some("board"), Some("search")) => Some(50),
            _ => None,
        }
    }

    /// Token budget for a board or feedback command when `--budget` is absent.
    pub fn default_budget(words: &[String]) -> usize {
        if words.first().map(String::as_str) == Some("board")
            && matches!(words.get(1).map(String::as_str), Some("show" | "review"))
        {
            4_000
        } else {
            1_500
        }
    }
}
