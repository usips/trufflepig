//! Parsed client options and explicit daemon argument forwarding.
use crate::{board::board_grammar::BoardOptions, output::OutputFormat, results};
use anyhow::{Result, bail};
use clap::Parser;
use std::path::{Path, PathBuf};

/// Largest serialized response budget in `o200k_base` tokens.
pub const MAX_BUDGET: usize = 1_000_000;

/// Query and navigation summary printed after the option list by `--help`.
const QUERY_HELP: &str = "\
Queries (search TEXT):
  words            identifier, lexical, and filename evidence (semantic when enabled)
  sym:NAME         exact, case-sensitive definition
  re:REGEX         regex over current file bytes
  file:PATH        root-relative path prefix, else a match at any path component
                   (file:script/host finds crates/a/src/script/host.rs); repeats OR
  -file:PATH       exclude paths matching at any component
                   (quote the query: search 'needle -file:tests')
  lang:L           rust, ts, js, csharp (cs, c#), php, luau, dm, text (md, toml, json)
  kind:K           function, struct, file, ...
  ws:home|ws:all   workspace scope; in:MEMBER selects one member

Navigation:
  show HANDLE | show path:FILE:START-END   numbered, verified source
  show 'sym:NAME'  a definition's full body in one call
  ctx HANDLE       relationships around a hit
  refs NAME        occurrences and resolved targets
  map PREFIX       module and type outline (a single file adds its functions)
  more CURSOR      continue with `trufflepig more CURSOR` using the cursor after `next:`
  show CURSOR      continue with `trufflepig show CURSOR` using the cursor after `next:`

Pages:
  -n N             hits per page (default 20)
  -b N             output token budget (default 600; show 1500)
\n\
Plans:
  board hello MODEL [EFFORT]  identify this session
  board inbox [SEQ] [--wait]  plan events and questions
  board show [P7|P7.3|P7@12|E512]  bounded overview or one plan, task, revision, or entry
  board feed [P7] [SEQ]      frozen event pages
  board attention [--all]   pending work for this actor
  board history P7 [SEQ]    immutable plan revision history
  board search TEXT... [--plan P7]  board full-text search
  board web [TARGET]        print the running console URL
  board-serve [--listen 127.0.0.1:7341]  serve the board console
  board claim P7.3 SCOPE...  claim a task before working
  board post P7 KIND TEXT...  post progress or ask a question
  board task P7.3 COLUMN    move a task and release its claim
  board propose P7@12 --body FILE SUMMARY...  propose a plan revision
  board review P7@12 [AGENT]  assemble review evidence
  feedback KIND SUMMARY... [--body FILE]  report a workaround
  board show/review default to 4000 tokens; other board/feedback to 1500";

const KNOWN_COMMANDS: &[&str] = &[
    "search",
    "show",
    "more",
    "ctx",
    "refs",
    "map",
    "index",
    "init",
    "status",
    "doctor",
    "hist-index",
    "hist-status",
    "hist",
    "since",
    "diff",
    "blame",
    "session",
    "audit",
    "forget-logs",
    "semantic-check",
    "semantic",
    "semantic-worker-serve",
    "ws",
    "system",
    "board",
    "board-serve",
    "feedback",
    "serve",
    "history-serve",
    "workspace-serve",
    "system-serve",
    "stop",
];

#[derive(Parser, Debug, Clone)]
#[command(
    version,
    about = "Repository source search with immutable handles and verified reads",
    after_help = QUERY_HELP
)]
/// Parsed command-line options for local and daemon-routed requests.
pub struct Arguments {
    /// Read-only capability used by board service installers.
    #[arg(long, hide = true)]
    pub board_api_version: bool,
    /// Repository root to index or inspect; defaults to the current directory.
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
    /// Explicit workspace configuration to use instead of discovery.
    #[arg(long, conflicts_with = "no_workspace")]
    pub workspace: Option<PathBuf>,
    /// Disable workspace discovery and force single-repository operation.
    #[arg(long)]
    pub no_workspace: bool,
    /// Select a workspace member for search or an owner-scoped command.
    #[arg(long)]
    pub member: Option<String>,
    /// Internal record of whether the caller supplied `--root` explicitly.
    #[arg(skip)]
    pub explicit_root: bool,
    /// Internal record of whether the caller supplied `--budget` explicitly.
    #[arg(skip)]
    pub explicit_budget: bool,
    /// Internal marker preserving an implicitly selected root when forwarding.
    #[arg(long, hide = true)]
    pub implicit_root: bool,
    /// Loopback HTTP address for the foreground board server.
    #[arg(long)]
    pub listen: Option<std::net::SocketAddr>,
    /// Override the cache directory.
    #[arg(long)]
    pub cache: Option<PathBuf>,
    /// Maximum serialized output budget in o200k_base tokens.
    #[arg(short = 'b', long, default_value_t = 600)]
    pub budget: usize,
    /// Maximum number of result hits per page.
    #[arg(short = 'n', long, default_value_t = 20)]
    pub limit: usize,
    /// Enable semantic retrieval when the semantic feature is available.
    #[arg(long, conflicts_with = "no_sem")]
    pub sem: bool,
    /// Disable semantic retrieval, including a workspace's persistent opt-in.
    #[arg(long, conflicts_with = "sem")]
    pub no_sem: bool,
    /// Enable cross-encoder reranking of the top fused hits when available.
    #[arg(long, conflicts_with = "no_rerank")]
    pub rerank: bool,
    /// Disable reranking, including a workspace's persistent opt-in.
    #[arg(long, conflicts_with = "rerank")]
    pub no_rerank: bool,
    /// Request JSON responses, which are the default output format.
    #[arg(long)]
    pub json: bool,
    /// Output format for search, refs, map, more, and show: `json` or `lines`.
    #[arg(long, default_value = "json", value_parser = ["json", "lines"])]
    pub format: String,
    /// Run locally without starting or using a daemon.
    #[arg(long)]
    pub no_daemon: bool,
    /// Override the shared history cache directory.
    #[arg(long)]
    pub history_cache: Option<PathBuf>,
    /// Internal resolved history cache forwarded to daemon and worker processes.
    #[arg(long, hide = true)]
    pub resolved_history_cache: Option<PathBuf>,
    /// Wait for a history index or semantic preparation operation to finish.
    #[arg(long)]
    pub wait: bool,
    /// Include uncommitted working-tree changes in `since`.
    #[arg(long)]
    pub uncommitted: bool,
    /// Restrict `diff` to a path, symbol, or result handle.
    #[arg(long)]
    pub target: Option<String>,
    /// Select the historical source side: `before` or `after`.
    #[arg(long, value_parser = ["before", "after"])]
    pub side: Option<String>,
    /// Use blame without ignoring whitespace or detecting moved/copied lines.
    #[arg(long)]
    pub raw: bool,
    /// File containing revisions for Git blame to ignore.
    #[arg(long)]
    pub ignore_revs_file: Option<PathBuf>,
    /// Diagnostic recording mode: `off`, `metadata`, or `detailed`.
    #[arg(long, default_value = "metadata", value_parser = ["off", "metadata", "detailed"])]
    pub diagnostics: String,
    /// Diagnostic session identifier to attach to this request.
    #[arg(long)]
    pub session: Option<String>,
    /// Label for the diagnostic client making this request.
    #[arg(long)]
    pub client: Option<String>,
    /// Plan board and feedback command options.
    #[command(flatten)]
    pub board: BoardOptions,
    /// Command and arguments; omit them for `status`, or use `search TEXT...` for a query.
    #[arg(num_args=0..)]
    pub words: Vec<String>,
}

impl Arguments {
    /// The page format selected by `--format`; validation rejects other values.
    pub fn output_format(&self) -> OutputFormat {
        OutputFormat::parse(&self.format).unwrap_or_default()
    }
}

/// Parse command-line arguments without performing repository validation.
pub fn parse(args: &[String]) -> Result<Arguments> {
    if let Some(answer) = super::board_api_probe::probe_board_api(args) {
        answer?;
    }
    let mut options = Arguments::try_parse_from(
        std::iter::once("trufflepig".to_owned()).chain(args.iter().cloned()),
    )?;
    options.explicit_root = !options.implicit_root
        && args
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| arg == "--root" || arg.starts_with("--root="));
    options.explicit_budget = args
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| {
            arg == "--budget"
                || arg.starts_with("--budget=")
                || arg == "-b"
                || arg.strip_prefix("-b").is_some_and(|value| {
                    let value = value.strip_prefix('=').unwrap_or(value);
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                })
        });
    let explicit_limit = args
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| {
            arg == "--limit"
                || arg.starts_with("--limit=")
                || arg == "-n"
                || arg.strip_prefix("-n").is_some_and(|value| {
                    let value = value.strip_prefix('=').unwrap_or(value);
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                })
        });
    if !explicit_limit {
        match (
            options.words.first().map(String::as_str),
            options.words.get(1).map(String::as_str),
        ) {
            (Some("board"), Some("feed" | "attention" | "history"))
            | (Some("feedback"), Some("ls")) => options.limit = 200,
            (Some("board"), Some("show")) if options.words.len() == 2 => options.limit = 200,
            (Some("board"), Some("search")) => options.limit = 50,
            _ => {}
        }
    }
    if !options.explicit_budget {
        if options.is_board() {
            options.budget = if options.words.first().map(String::as_str) == Some("board")
                && matches!(
                    options.words.get(1).map(String::as_str),
                    Some("show" | "review")
                ) {
                4_000
            } else {
                1_500
            };
        } else if options.is_show() {
            options.budget = SHOW_BUDGET;
        }
    }
    Ok(options)
}

/// Default `show` budget: enough for a typical definition body in one read.
pub const SHOW_BUDGET: usize = 1_500;

impl Arguments {
    /// Whether the command reads source, which defaults to `SHOW_BUDGET`.
    pub fn is_show(&self) -> bool {
        self.words.first().map(String::as_str) == Some("show")
    }

    /// Foreground board HTTP binding; loopback is enforced before serving.
    pub fn board_listen_address(&self) -> std::net::SocketAddr {
        self.listen
            .unwrap_or_else(|| std::net::SocketAddr::from(([127, 0, 0, 1], 7341)))
    }

    /// Whether the request uses the plan board edge.
    pub fn is_board(&self) -> bool {
        matches!(
            self.words.first().map(String::as_str),
            Some("board" | "feedback")
        )
    }
}

pub(super) fn validate(options: &Arguments) -> Result<()> {
    options
        .board
        .validate_for_verb(options.words.first().map(String::as_str))?;
    if let Some(command) = options.words.first().map(String::as_str)
        && !KNOWN_COMMANDS.contains(&command)
    {
        bail!(
            "unknown_command: {command}; {}",
            unknown_command_hint(command)
        );
    }
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    if options.no_daemon && (verb == "serve" || verb.ends_with("-serve")) {
        bail!("invalid_options: --no-daemon cannot start a server");
    }
    if options.listen.is_some() && verb != "board-serve" {
        bail!("invalid_options: --listen requires board-serve");
    }
    if verb == "board-serve" {
        crate::board::board_grammar::validate_board_surface(options)?;
        if options.words.len() != 1 {
            bail!("usage: board-serve [--listen 127.0.0.1:7341]");
        }
        let address = options.board_listen_address();
        if !address.ip().is_loopback() {
            bail!("invalid_options: board-serve must listen on a loopback address");
        }
    }
    if options.json && options.format != "json" {
        bail!("invalid_options: --json conflicts with --format lines");
    }
    if options.limit == 0 || options.limit > results::MAX_HITS {
        bail!("invalid_limit: expected 1..10000");
    }
    if options.budget > MAX_BUDGET {
        bail!("invalid_budget: maximum is {MAX_BUDGET} tokens");
    }
    Ok(())
}

/// What an unknown first word most likely meant.
fn unknown_command_hint(command: &str) -> String {
    if matches!(command, "next" | "page" | "cursor" | "continue") {
        "page with `more CURSOR`, the footer's `next: more CURSOR` line".to_owned()
    } else if command.parse::<crate::identity::ResultCursor>().is_ok() {
        format!("use `more {command}` to page")
    } else if command.parse::<crate::identity::ResultHandle>().is_ok() {
        format!("use `show {command}` to read a hit")
    } else {
        format!("use `search {command}` for a free-form query")
    }
}

pub(crate) fn normalized_args(options: &Arguments, root: &Path) -> Vec<String> {
    let mut args = vec![
        "--root".into(),
        root.to_string_lossy().into_owned(),
        "--budget".into(),
        options.budget.to_string(),
        "--limit".into(),
        options.limit.to_string(),
    ];
    if !options.explicit_root {
        args.push("--implicit-root".into());
    }
    if options.sem {
        args.push("--sem".into());
    }
    if options.no_sem {
        args.push("--no-sem".into());
    }
    if options.rerank {
        args.push("--rerank".into());
    }
    if options.no_rerank {
        args.push("--no-rerank".into());
    }
    for (flag, value) in [
        (
            "--cache",
            options
                .cache
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        ),
        (
            "--history-cache",
            options
                .history_cache
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        ),
        (
            "--resolved-history-cache",
            options
                .resolved_history_cache
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        ),
        ("--target", options.target.clone()),
        ("--side", options.side.clone()),
        (
            "--ignore-revs-file",
            options
                .ignore_revs_file
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        ),
        ("--diagnostics", Some(options.diagnostics.clone())),
        ("--format", Some(options.format.clone())),
    ] {
        if let Some(value) = value {
            args.extend([flag.into(), value]);
        }
    }
    for (flag, enabled) in [
        ("--wait", options.is_board() && options.wait),
        ("--uncommitted", options.uncommitted),
        ("--raw", options.raw),
        ("--no-daemon", options.no_daemon),
    ] {
        if enabled {
            args.push(flag.into());
        }
    }
    if let Some(path) = &options.workspace {
        args.extend(["--workspace".into(), path.to_string_lossy().into_owned()]);
    }
    if options.no_workspace {
        args.push("--no-workspace".into());
    }
    if let Some(member) = &options.member {
        args.extend(["--member".into(), member.clone()]);
    }
    options.board.forward(&mut args);
    args.extend(options.words.iter().cloned());
    args
}
