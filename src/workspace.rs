//! Explicit multi-root retrieval with persisted ownership and one final output budget.
pub mod config;
mod coordinator;
mod navigation;
mod owned_response;
mod result_cache;
mod retrieval;
use crate::{
    cli::Arguments, diagnostics::RequestContext, output::OutputBudget, store::encode_path,
};
use anyhow::{Context, Result, bail, ensure};
use config::{Member, WorkspaceConfig};
pub use coordinator::run;
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
                .members
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
pub fn member_cache(member: &Member, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(base) = explicit {
        return Ok(base.join("members").join(
            &blake3::hash(encode_path(&member.root).as_bytes())
                .to_hex()
                .as_str()[..24],
        ));
    }
    crate::cli::cache_path(&member.root, None)
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
            config.home(&root).or_else(|| {
                options
                    .member
                    .as_ref()
                    .and_then(|name| config.members.iter().find(|m| &m.name == name))
            })
        };
        if let Some(member) = member {
            return Ok((
                member.root.clone(),
                member_cache(member, options.cache.as_deref())?,
                Some(config.id.clone()),
                Some(member.name.clone()),
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

fn member_options(options: &Arguments, member: &Member) -> Result<Arguments> {
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
    Ok(local)
}
fn selected_member<'a>(config: &'a WorkspaceConfig, options: &Arguments) -> Result<&'a Member> {
    if let Some(name) = &options.member {
        return config
            .members
            .iter()
            .find(|m| &m.name == name)
            .context("invalid_member: member is not configured");
    }
    config
        .home(&options.root.canonicalize()?)
        .context("member_required: select --member for an owner-scoped command")
}
pub(crate) fn local(
    config: &WorkspaceConfig,
    cache: &Path,
    options: &Arguments,
    context: &RequestContext,
    session: &mut crate::semantic::SemanticSession,
) -> Result<String> {
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    let budget = OutputBudget::new(options.budget)?;
    if verb == "ws" {
        return inspect(config, options, &budget);
    }
    let results = WorkspaceResults::open(cache)?;
    match verb {
        "more" => results.more(
            options.words.get(1).context("usage: more SET@OFFSET")?,
            options.limit,
            &budget,
        ),
        "show" | "ctx" => navigation::read(config, &results, options, &budget),
        "hist" | "since" | "diff" | "blame" => {
            navigation::history(config, &results, options, &budget)
        }
        "index" | "init" | "status" | "doctor" | "hist-index" | "hist-status" | "session"
        | "audit" | "forget-logs" | "semantic-check" => {
            let member = selected_member(config, options)?;
            member.verify_identity()?;
            let mut local = member_options(options, member)?;
            local.budget = 1_000_000;
            let mut args = crate::cli::normalized_args(&local, &local.root);
            if options.wait {
                args.push("--wait".into());
            }
            let mut value: Value =
                serde_json::from_str(&crate::cli::run_with_context(&args, context)?)?;
            // Owner commands budget their payload again after adding mandatory provenance.
            value["member"] = member.name.clone().into();
            value["repository"] = encode_path(&member.root).into();
            value["workspace"] = config.name.clone().into();
            owned_response::render(value, &budget)
        }
        "serve" | "history-serve" | "workspace-serve" => {
            bail!("invalid_command: internal server command")
        }
        _ => retrieval::search(config, cache, &results, options, context, session),
    }
}
fn inspect(config: &WorkspaceConfig, options: &Arguments, budget: &OutputBudget) -> Result<String> {
    let command = options.words.get(1).map(String::as_str).unwrap_or("show");
    ensure!(
        matches!(command, "show" | "status"),
        "usage: ws show | ws status | ws discover PATH..."
    );
    let mut members = Vec::with_capacity(config.members.len());
    for member in &config.members {
        let cache = member_cache(member, options.cache.as_deref())?;
        let mut value = json!({"member":member.name,"root":encode_path(&member.root),"available":member.verify_identity().is_ok()});
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
    budget.render(&json!({"workspace":config.name,"config":encode_path(&config.path),"home":config.home(&options.root.canonicalize()?).map(|m| &m.name),"members":members}))
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
