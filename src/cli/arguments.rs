//! Parsed client options and explicit daemon argument forwarding.
use crate::results;
use anyhow::{Result, bail};
use clap::Parser;
use std::path::{Path, PathBuf};

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
    "serve",
    "history-serve",
    "workspace-serve",
    "stop",
];

#[derive(Parser, Debug, Clone)]
#[command(
    version,
    about = "Repository source search with immutable handles and verified reads"
)]
/// Parsed command-line options for local and daemon-routed requests.
pub struct Arguments {
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
    /// Internal marker preserving an implicitly selected root when forwarding.
    #[arg(long, hide = true)]
    pub implicit_root: bool,
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
    /// Request JSON responses, which are the default output format.
    #[arg(long)]
    pub json: bool,
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
    /// Command and arguments; omit them for `status`, or use `search TEXT...` for a query.
    #[arg(num_args=0..)]
    pub words: Vec<String>,
}

/// Parse command-line arguments without performing repository validation.
pub fn parse(args: &[String]) -> Result<Arguments> {
    let mut options = Arguments::try_parse_from(
        std::iter::once("trufflepig".to_owned()).chain(args.iter().cloned()),
    )?;
    options.explicit_root = !options.implicit_root
        && args
            .iter()
            .any(|arg| arg == "--root" || arg.starts_with("--root="));
    Ok(options)
}

pub(super) fn validate(options: &Arguments) -> Result<()> {
    if let Some(command) = options.words.first().map(String::as_str)
        && !KNOWN_COMMANDS.contains(&command)
    {
        bail!("unknown_command: {command}; use `search {command}` for a free-form query");
    }
    if options.no_daemon
        && options.words.first().is_some_and(|verb| {
            matches!(verb.as_str(), "serve" | "history-serve" | "workspace-serve")
        })
    {
        bail!("invalid_options: --no-daemon cannot start a server");
    }
    if options.limit == 0 || options.limit > results::MAX_HITS {
        bail!("invalid_limit: expected 1..10000");
    }
    if options.budget > 1_000_000 {
        bail!("invalid_budget: maximum is 1000000 tokens");
    }
    Ok(())
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
    ] {
        if let Some(value) = value {
            args.extend([flag.into(), value]);
        }
    }
    for (flag, enabled) in [
        ("--wait", false),
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
    args.extend(options.words.iter().cloned());
    args
}
