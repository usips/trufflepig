//! Parsed client options and explicit daemon argument forwarding.
use crate::results;
use anyhow::{Result, bail};
use clap::Parser;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug, Clone)]
#[command(
    version,
    about = "Repository source search with immutable handles and verified reads"
)]
pub struct Arguments {
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
    #[arg(long, conflicts_with = "no_workspace")]
    pub workspace: Option<PathBuf>,
    #[arg(long)]
    pub no_workspace: bool,
    #[arg(long)]
    pub member: Option<String>,
    #[arg(skip)]
    pub explicit_root: bool,
    #[arg(long, hide = true)]
    pub implicit_root: bool,
    #[arg(long)]
    pub cache: Option<PathBuf>,
    #[arg(short = 'b', long, default_value_t = 600)]
    pub budget: usize,
    #[arg(short = 'n', long, default_value_t = 20)]
    pub limit: usize,
    #[arg(long)]
    pub sem: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub no_daemon: bool,
    #[arg(long)]
    pub history_cache: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub resolved_history_cache: Option<PathBuf>,
    #[arg(long)]
    pub wait: bool,
    #[arg(long)]
    pub uncommitted: bool,
    #[arg(long)]
    pub target: Option<String>,
    #[arg(long, value_parser = ["before", "after"])]
    pub side: Option<String>,
    #[arg(long)]
    pub raw: bool,
    #[arg(long)]
    pub ignore_revs_file: Option<PathBuf>,
    #[arg(long, default_value = "metadata", value_parser = ["off", "metadata", "detailed"])]
    pub diagnostics: String,
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub client: Option<String>,
    #[arg(num_args=0..)]
    pub words: Vec<String>,
}

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
