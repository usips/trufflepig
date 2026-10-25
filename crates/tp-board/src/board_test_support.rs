//! Board-only test helpers; disk scratch lives in `tp_core::test_fixtures`.
use crate::board::board_grammar::{BoardArguments, BoardOptions};
use std::io::Read;
use std::path::PathBuf;
use tp_core::output::OutputFormat;

/// Reads into `buffer`, retrying interrupted syscalls: signal tests run
/// beside blocking test reads under the full suite, so a raw `read` can
/// return `ErrorKind::Interrupted`, which callers must retry.
pub(crate) fn read_ignoring_interrupts(
    stream: &mut std::net::TcpStream,
    buffer: &mut [u8],
) -> std::io::Result<usize> {
    loop {
        match stream.read(buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

/// The board slice of the global command line, for tests that start from argv.
#[derive(clap::Parser)]
#[command(no_binary_name = true)]
struct BoardCommandLine {
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[arg(short = 'b', long)]
    budget: Option<usize>,
    #[arg(short = 'n', long)]
    limit: Option<usize>,
    #[arg(long, default_value = "json", value_parser = ["json", "lines"])]
    format: String,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    wait: bool,
    #[arg(long)]
    no_daemon: bool,
    #[arg(long)]
    sem: bool,
    #[arg(long)]
    no_sem: bool,
    #[arg(long)]
    rerank: bool,
    #[arg(long)]
    no_rerank: bool,
    #[arg(long)]
    member: Option<String>,
    #[arg(long)]
    cache: Option<PathBuf>,
    #[arg(long)]
    no_cache: bool,
    #[command(flatten)]
    board: BoardOptions,
    #[arg(num_args = 0..)]
    words: Vec<String>,
}

/// Parses board argv with the CLI's board defaults for budget and limit.
pub(crate) fn parse_board_arguments(args: &[String]) -> anyhow::Result<BoardArguments> {
    use clap::Parser;
    let line = BoardCommandLine::try_parse_from(args)?;
    let search_flag = [
        ("sem", line.sem),
        ("no-sem", line.no_sem),
        ("rerank", line.rerank),
        ("no-rerank", line.no_rerank),
        ("member", line.member.is_some()),
        ("cache", line.cache.is_some()),
        ("no-cache", line.no_cache),
    ]
    .into_iter()
    .find_map(|(flag, present)| present.then_some(flag));
    let board_words = matches!(
        line.words.first().map(String::as_str),
        Some("board" | "feedback")
    );
    let budget = line.budget.unwrap_or(if board_words {
        BoardArguments::default_budget(&line.words)
    } else {
        600
    });
    let limit = line
        .limit
        .or_else(|| BoardArguments::default_limit(&line.words))
        .unwrap_or(20);
    Ok(BoardArguments {
        format: OutputFormat::parse(&line.format)?,
        words: line.words,
        board: line.board,
        root: line.root,
        budget,
        limit,
        wait: line.wait,
        no_daemon: line.no_daemon,
        search_flag,
    })
}

/// Encodes `options` as router argv and parses it back, as the router would.
pub(crate) fn round_trip(options: &BoardArguments) -> BoardArguments {
    parse_board_arguments(&board_argv(options)).unwrap()
}

/// The router argv for `options`, in the CLI's forwarding order.
pub(crate) fn board_argv(options: &BoardArguments) -> Vec<String> {
    let format = match options.format {
        OutputFormat::Json => "json",
        OutputFormat::Lines => "lines",
    };
    let mut args = vec![
        "--root".to_owned(),
        options.root.to_string_lossy().into_owned(),
        "--budget".to_owned(),
        options.budget.to_string(),
        "--limit".to_owned(),
        options.limit.to_string(),
        "--format".to_owned(),
        format.to_owned(),
    ];
    if options.wait {
        args.push("--wait".to_owned());
    }
    if options.no_daemon {
        args.push("--no-daemon".to_owned());
    }
    args.extend(options.words.iter().cloned());
    options.board.forward(&mut args);
    args
}
