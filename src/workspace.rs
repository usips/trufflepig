//! Explicit multi-root retrieval with persisted ownership and one final output budget.
pub mod config;
mod coordinator;
pub mod member_root;
mod navigation;
mod owned_response;
mod result_cache;
mod retrieval;
mod worktree_cache;
use crate::{
    cli::Arguments, diagnostics::RequestContext, output::OutputBudget, store::encode_path,
};
use anyhow::{Context, Result, bail, ensure};
use config::WorkspaceConfig;
pub(crate) use coordinator::apply_config;
pub use coordinator::run;
use member_root::MemberRoot;
use result_cache::WorkspaceResults;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn resolve(options: &Arguments) -> Result<Option<WorkspaceConfig>> {
    let start = options
        .root
        .canonicalize()
        .context("invalid_root: cannot resolve workspace home")?;
    let config = config::discover(
        &start,
        options.workspace.as_deref(),
        options.no_workspace,
        options.explicit_root,
    )?;
    if let (Some(config), Some(cache)) = (&config, &options.cache) {
        let base = config::resolve_path(cache, &std::env::current_dir()?)?;
        ensure!(
            config
                .member_roots(&start)?
                .iter()
                .all(|member| !base.starts_with(&member.root) && !member.root.starts_with(&base)),
            "invalid_cache: workspace cache base must be outside member roots"
        );
    }
    Ok(config)
}
pub fn cache_path(config: &WorkspaceConfig, explicit: Option<&Path>) -> Result<PathBuf> {
    match explicit {
        Some(base) => Ok(base.join("workspace").join(&config.id[..24])),
        None => {
            let root_cache = crate::cli::cache_path(&config.path, None)?;
            Ok(root_cache
                .parent()
                .context("invalid cache base")?
                .join("workspaces")
                .join(&config.id[..24]))
        }
    }
}
/// Cache directory for a member's effective root; a worktree root is warmed
/// from its configured member's cache.
pub fn member_cache(member: &MemberRoot, explicit: Option<&Path>) -> Result<PathBuf> {
    if member.worktree.is_some() {
        return worktree_cache::ensure(&member.member.root, &member.root, explicit);
    }
    hashed_member_cache(&member.root, explicit)
}
fn hashed_member_cache(root: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(base) = explicit {
        return Ok(base
            .join("members")
            .join(&blake3::hash(encode_path(root).as_bytes()).to_hex().as_str()[..24]));
    }
    crate::cli::cache_path(root, None)
}
/// Captures the request's diagnostic home before execution or configuration changes.
pub(crate) fn diagnostic_location(
    options: &Arguments,
) -> Result<(PathBuf, PathBuf, Option<String>, Option<String>)> {
    if let Some(config) = resolve(options)? {
        let root = options.root.canonicalize()?;
        let scoped = options
            .words
            .first()
            .is_some_and(|v| matches!(v.as_str(), "session" | "audit" | "forget-logs"));
        let member = if scoped {
            Some(selected_member(&config, options)?)
        } else {
            match config.home_root(&root)? {
                Some(home) => Some(home),
                None => options
                    .member
                    .as_ref()
                    .and_then(|name| config.members.iter().find(|m| &m.name == name))
                    .map(MemberRoot::configured),
            }
        };
        if let Some(member) = member {
            return Ok((
                member.root.clone(),
                member_cache(&member, options.cache.as_deref())?,
                Some(config.id.clone()),
                Some(member.name().to_owned()),
            ));
        }
        return Ok((
            root,
            cache_path(&config, options.cache.as_deref())?,
            Some(config.id),
            None,
        ));
    }
    let root = options.root.canonicalize()?;
    let cache = crate::cli::cache_path(&root, options.cache.as_deref())?;
    Ok((root, cache, None, None))
}

fn member_options(options: &Arguments, member: &MemberRoot) -> Result<Arguments> {
    let mut local = options.clone();
    local.root = member.root.clone();
    local.cache = Some(member_cache(member, options.cache.as_deref())?);
    local.resolved_history_cache = crate::history::worker::resolve_cache(
        &member.root,
        options
            .cache
            .as_ref()
            .map(|_| local.cache.as_deref().expect("member cache")),
        options.history_cache.as_deref(),
    )
    .ok();
    local.workspace = None;
    local.no_workspace = true;
    local.member = None;
    // Owner-forwarded and history sub-requests are re-parsed as JSON.
    local.format = "json".into();
    Ok(local)
}
/// The member an owner-scoped command targets: `--member`, else home, which
/// may be a linked worktree standing in for its member.
fn selected_member(config: &WorkspaceConfig, options: &Arguments) -> Result<MemberRoot> {
    let start = options.root.canonicalize()?;
    if let Some(name) = &options.member {
        return config
            .member_roots(&start)?
            .into_iter()
            .find(|m| m.name() == name)
            .context("invalid_member: member is not configured");
    }
    config
        .home_root(&start)?
        .context("member_required: select --member for an owner-scoped command")
}
pub(crate) fn local(
    config: &WorkspaceConfig,
    cache: &Path,
    options: &Arguments,
    context: &RequestContext,
    session: &mut crate::semantic::SemanticSession,
) -> Result<String> {
    session.set_no_daemon(options.no_daemon);
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    let budget = OutputBudget::new(options.budget)?;
    if verb == "semantic" {
        return crate::cli::semantic::workspace(config, options, context);
    }
    if verb == "ws" {
        return inspect(config, options, &budget);
    }
    let results = WorkspaceResults::open(cache)?;
    let page_budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    match verb {
        "more" => results.more(
            options.words.get(1).context("usage: more SET@OFFSET")?,
            options.limit,
            &page_budget,
        ),
        "show" => navigation::read(config, &results, options, &page_budget),
        "ctx" => navigation::read(config, &results, options, &budget),
        "hist" | "since" | "diff" | "blame" => {
            navigation::history(config, &results, options, &budget)
        }
        "index" | "init" | "status" | "doctor" | "hist-index" | "hist-status" | "session"
        | "audit" | "forget-logs" | "semantic-check" => {
            let member = selected_member(config, options)?;
            member.verify_identity()?;
            let mut local = member_options(options, &member)?;
            local.budget = 1_000_000;
            let mut args = crate::cli::normalized_args(&local, &local.root);
            if options.wait {
                args.push("--wait".into());
            }
            let mut value: Value =
                serde_json::from_str(&crate::cli::run_with_context(&args, context)?)?;
            // Owner commands budget their payload again after adding mandatory provenance.
            value["member"] = member.name().into();
            value["repository"] = encode_path(&member.root).into();
            if let Some(label) = &member.worktree {
                value["worktree"] = label.clone().into();
            }
            value["workspace"] = config.name.clone().into();
            owned_response::render(value, &budget)
        }
        "serve" | "history-serve" | "workspace-serve" => {
            bail!("invalid_command: internal server command")
        }
        "search" | "refs" | "map" => {
            retrieval::search(config, cache, &results, options, context, session)
        }
        _ => bail!("invalid_command: unknown command {verb}; use search for queries"),
    }
}
fn inspect(config: &WorkspaceConfig, options: &Arguments, budget: &OutputBudget) -> Result<String> {
    let command = options.words.get(1).map(String::as_str).unwrap_or("show");
    ensure!(
        matches!(command, "show" | "status"),
        "usage: ws show | ws status | ws discover PATH..."
    );
    let start = options.root.canonicalize()?;
    let roots = config.member_roots(&start)?;
    let mut members = Vec::with_capacity(roots.len());
    for member in &roots {
        let cache = member_cache(member, options.cache.as_deref())?;
        let mut value = json!({"member":member.name(),"root":encode_path(&member.root),"available":member.verify_identity().is_ok()});
        if let Some(label) = &member.worktree {
            value["worktree"] = label.clone().into();
        }
        if command == "status"
            && member.verify_identity().is_ok()
            && cache.join("index.sqlite3").exists()
        {
            match crate::store::Store::open(&member.root, &cache) {
                Ok(store) => {
                    value["generation"] = store.generation()?.into();
                    value["coverage"] = serde_json::to_value(store.coverage()?)?;
                }
                Err(_) => value["available"] = false.into(),
            }
        }
        members.push(value);
    }
    let home = config.home_root(&start)?;
    budget.render(&json!({"workspace":config.name,"config":encode_path(&config.path),"home":home.as_ref().map(MemberRoot::name),"members":members}))
}
pub fn discover_paths(paths: &[String], budget: usize) -> Result<String> {
    ensure!(!paths.is_empty(), "usage: ws discover PATH...");
    ensure!(paths.len() <= 32, "invalid_workspace: maximum 32 members");
    let mut members = toml::map::Map::new();
    for path in paths {
        let root =
            config::resolve_path(Path::new(path), &std::env::current_dir()?)?.canonicalize()?;
        ensure!(
            root.is_dir(),
            "invalid_workspace: member must be a directory"
        );
        let name = root
            .file_name()
            .and_then(|s| s.to_str())
            .context("invalid_workspace: choose a member name explicitly")?;
        ensure!(
            config::valid_name(name) && !members.contains_key(name),
            "invalid_workspace: member name is invalid or duplicated; write named members explicitly"
        );
        let mut entry = toml::map::Map::new();
        entry.insert(
            "path".into(),
            toml::Value::String(
                root.to_str()
                    .context("invalid_workspace: config path is not UTF-8")?
                    .into(),
            ),
        );
        members.insert(name.into(), toml::Value::Table(entry));
    }
    let proposal = toml::to_string(&toml::Value::Table(toml::map::Map::from_iter([
        (
            "workspace".into(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "name".into(),
                toml::Value::String("workspace".into()),
            )])),
        ),
        ("members".into(), toml::Value::Table(members)),
    ])))?;
    OutputBudget::new(budget)?.render(&json!({"proposal":proposal,"applied":false}))
}
#[cfg(test)]
mod tests;
