//! Linux command dispatch; stdout contains one budgeted JSON response.
use crate::{daemon, output::OutputBudget, results, search, source, store::Store};
use anyhow::{Context, Result, bail};
use clap::Parser;
#[cfg(test)]
mod tests;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Repository source search with immutable handles and verified reads"
)]
pub struct Arguments {
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
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
    #[arg(num_args=0..)]
    pub words: Vec<String>,
}

fn parse(args: &[String]) -> Result<Arguments> {
    Ok(Arguments::try_parse_from(
        std::iter::once("trufflepig".to_owned()).chain(args.iter().cloned()),
    )?)
}

pub fn run(args: &[String]) -> Result<String> {
    let options = parse(args)?;
    validate(&options)?;
    let root = options
        .root
        .canonicalize()
        .context("invalid_root: cannot open repository root")?;
    let cache = cache_path(&root, options.cache.as_deref())?;
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    if verb == "serve" {
        let mut semantic_session = crate::semantic::SemanticSession::default();
        daemon::serve(&root, &cache, |request| match request {
            Some(args) => {
                local_with_session(&root, &cache, &parse(&args)?, true, &mut semantic_session)
            }
            None => {
                let mut store = Store::open(&root, &cache)?;
                store.index()?;
                Ok(String::new())
            }
        })?;
        return Ok(String::new());
    }
    if verb == "stop" {
        Store::open(&root, &cache)?;
        return OutputBudget::new(options.budget)?.render(&serde_json::from_str::<
            serde_json::Value,
        >(&daemon::stop(&cache)?)?);
    }
    if !options.no_daemon && !matches!(verb, "index" | "init" | "semantic-check" | "doctor") {
        if let Some(response) = daemon::request(&cache, &normalized_args(&options, &root))? {
            return Ok(response);
        }
        std::fs::create_dir_all(&cache)?;
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--root")
            .arg(&root)
            .arg("--cache")
            .arg(&cache)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(response) = daemon::request(&cache, &normalized_args(&options, &root))? {
                return Ok(response);
            }
            if child.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // A large startup scan may still own the server; local SQLite access remains coherent.
    }
    local(&root, &cache, &options, false)
}

fn cache_path(root: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("cache_unavailable: set --cache or XDG_CACHE_HOME")?;
    use std::os::unix::ffi::OsStrExt;
    Ok(base
        .join("trufflepig")
        .join(blake3::hash(root.as_os_str().as_bytes()).to_hex().as_str()))
}

pub fn local(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    daemon_running: bool,
) -> Result<String> {
    local_with_session(
        root,
        cache,
        options,
        daemon_running,
        &mut crate::semantic::SemanticSession::default(),
    )
}

fn local_with_session(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    daemon_running: bool,
    session: &mut crate::semantic::SemanticSession,
) -> Result<String> {
    validate(options)?;
    if options.root.canonicalize()? != root {
        bail!("invalid_root: daemon cache belongs to another repository");
    }
    let budget = OutputBudget::new(options.budget)?;
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    if verb == "semantic-check" {
        let path = options
            .words
            .get(1)
            .context("usage: semantic-check MODEL_DIRECTORY")?;
        return budget.render(&crate::semantic::run_gate(Path::new(path))?);
    }
    let mut store = Store::open(root, cache)?;
    let argument = || {
        options
            .words
            .get(1)
            .map(String::as_str)
            .context("usage: command requires an explicit argument")
    };
    match verb {
        "index"|"init"=>{let coverage=store.index()?;budget.render(&serde_json::json!({"generation":store.generation()?,"coverage":coverage}))},
        "status"|"doctor"=>budget.render(&serde_json::json!({"generation":store.generation()?,"coverage":store.coverage()?,"semantic_feature":cfg!(feature="semantic"),"tokenizer":"o200k_base"})),
        "show"=>source::show(&store,argument()?,&budget),
        "more"=>results::more(&store,argument()?,options.limit,&budget),
        "ctx"=>search::context(&store,argument()?,&budget),
        _=>{
            if !daemon_running || store.generation()?==0 {store.index()?;}
            let set=match verb {
                "refs"=>search::references(&store,argument()?)?,
                "map"=>search::map(&store,options.words.get(1).map(String::as_str).unwrap_or(""))?,
                _=>{
                    let text=if verb=="search" {options.words[1..].join(" ")}else{options.words.join(" ")};
                    if let Some(name)=text.strip_prefix("refs:"){search::references(&store,name)?}
                    else{search::search_with_session(&store,&search::Query::parse(&text)?,options.sem,cache,session)?}
                }
            };
            let id=results::save(&mut store,set)?;
            results::page(&store,&id,0,options.limit,&budget)
        }
    }
}

fn validate(options: &Arguments) -> Result<()> {
    if options.limit == 0 || options.limit > results::MAX_HITS {
        bail!("invalid_limit: expected 1..10000");
    }
    if options.budget > 1_000_000 {
        bail!("invalid_budget: maximum is 1000000 tokens");
    }
    Ok(())
}

fn normalized_args(options: &Arguments, root: &Path) -> Vec<String> {
    let mut args = vec![
        "--root".into(),
        root.to_string_lossy().into_owned(),
        "--budget".into(),
        options.budget.to_string(),
        "--limit".into(),
        options.limit.to_string(),
    ];
    if options.sem {
        args.push("--sem".into());
    }
    args.extend(options.words.iter().cloned());
    args
}
