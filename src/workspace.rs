//! Explicit multi-root retrieval with persisted ownership and one final output budget.
mod coordinator;
mod current_path_hints;
mod home_index;
pub mod member_root;
mod owned_response;
mod owner_navigation;
mod result_cache;
mod retrieval;
mod stateless_queries;
#[cfg(test)]
mod test_fixture;
pub(crate) mod workspace_discovery;
mod workspace_list;
mod worktree_cache;
use crate::{cli::Arguments, search::InvocationDirectory, store::Store};
use anyhow::{Context, Result, bail, ensure};
pub(crate) use coordinator::{apply_config, run_with_deadline};
pub(crate) use home_index::{
    ParentFallback, ParentIndexIdentity, ParentIndexView, WorktreeHashes, acquire_through_parent,
    reextracted_entry, standalone_parent_view,
};
use member_root::MemberRoot;
use result_cache::WorkspaceResults;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tp_core::coded_error::CodedContext;
use tp_core::workspace_config::{self, WorkspaceConfig};
use tp_core::{
    output::OutputBudget, path_codec::encode_path, query_deadline::QueryDeadline,
    request_context::RequestContext,
};
use workspace_discovery::WorkspaceMembers;
pub(crate) use workspace_list::list;

pub fn resolve(options: &Arguments) -> Result<Option<WorkspaceConfig>> {
    let start = options
        .root
        .canonicalize()
        .with_coded_context(|| coded!(InvalidRoot, "cannot resolve workspace home"))?;
    let config = workspace_discovery::discover(
        &start,
        options.workspace.as_deref(),
        options.no_workspace,
        options.explicit_root,
    )?;
    if let (Some(config), Some(cache)) = (&config, &options.cache) {
        let base = workspace_config::resolve_path(cache, &std::env::current_dir()?)?;
        ensure!(
            config
                .member_roots(&start)?
                .iter()
                .all(|member| !base.starts_with(&member.root) && !member.root.starts_with(&base)),
            coded!(
                InvalidCache,
                "workspace cache base must be outside member roots"
            )
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
/// Cache directory for a member read, prepared within its accepted request budget.
pub(super) fn member_cache_until(
    member: &MemberRoot,
    explicit: Option<&Path>,
    deadline: &QueryDeadline,
) -> Result<PathBuf> {
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let cache = if member.worktree.is_some() {
        worktree_cache::ensure_until(&member.member.root, &member.root, explicit, deadline)
    } else {
        hashed_member_cache(&member.root, explicit)
    }?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    Ok(cache)
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
                hashed_member_cache(&member.root, options.cache.as_deref())?,
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

fn is_local_query(options: &Arguments) -> bool {
    match options.words.first().map(String::as_str) {
        None => true,
        Some("semantic") => {
            let command = options.words.get(1).map(String::as_str);
            command == Some("status")
                || command == Some("worker")
                    && options.words.get(2).is_some_and(|word| word == "status")
        }
        Some(
            "search" | "refs" | "map" | "show" | "more" | "ctx" | "status" | "doctor" | "hist"
            | "hist-status" | "since" | "diff" | "blame" | "session" | "audit" | "ws",
        ) => true,
        _ => false,
    }
}

fn local_execution_deadline(options: &Arguments, incoming: &QueryDeadline) -> QueryDeadline {
    if is_local_query(options) {
        incoming.capped(tp_core::query_deadline::QUERY_DEADLINE)
    } else {
        incoming.clone()
    }
}

fn member_options(
    options: &Arguments,
    member: &MemberRoot,
    deadline: &QueryDeadline,
) -> Result<Arguments> {
    let mut local = options.clone();
    local.root = member.root.clone();
    let cache = if is_local_query(options) {
        member_cache_until(member, options.cache.as_deref(), deadline)?
    } else {
        member_cache(member, options.cache.as_deref())?
    };
    local.cache = Some(cache);
    local.resolved_history_cache = crate::history::history_worker::resolve_cache(
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
            .with_coded_context(|| coded!(InvalidMember, "member is not configured"));
    }
    config.home_root(&start)?.with_coded_context(|| {
        coded!(
            MemberRequired,
            "select --member for an owner-scoped command"
        )
    })
}

/// Reads a current path without opening or creating the workspace result cache.
pub(super) fn current_path_show(
    config: &WorkspaceConfig,
    options: &Arguments,
    deadline: &QueryDeadline,
) -> Result<Option<String>> {
    if options.words.first().is_none_or(|verb| verb != "show") {
        return Ok(None);
    }
    let Some(target) = crate::source::acquisition::show_target(&options.words) else {
        return Ok(None);
    };
    if !crate::source::acquisition::is_current_path_target(&target) {
        return Ok(None);
    }
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let member = if target.starts_with("read:path:") {
        stateless_queries::cursor_owner(config, options, &target, deadline)?
    } else {
        selected_member(config, options)?
    };
    member.verify_identity()?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );

    // This calculates the final cache path without the worktree helper's mkdir/seed side effects.
    let cache = hashed_member_cache(&member.root, options.cache.as_deref())?;
    let store = Store::unpublished(&member.root, &cache)?;
    let invocation = options.root.canonicalize()?;
    let origin = InvocationDirectory::within(&member.root, &invocation);
    let side = options
        .side
        .as_deref()
        .map(crate::source::SourceSide::parse)
        .transpose()?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let acquired = crate::source::acquisition::acquire_current_path_without_cache(
        &store, &target, side, &origin,
    )
    .map_err(|error| {
        current_path_hints::decorate_source_unavailable(config, &member, &target, error, deadline)
    })
    .map_err(|error| deadline.classify(error))?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );

    let mut metadata = json!({
        "workspace": config.name,
        "member": member.name(),
        "repository": encode_path(&member.root),
    });
    if let Some(label) = &member.worktree {
        metadata["worktree"] = label.clone().into();
    }
    let budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let rendered = crate::source::render_owned(acquired, &budget, &metadata)
        .map_err(|error| deadline.classify(error))?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    Ok(Some(rendered))
}

/// Runs one workspace command against an existing coordinator cache.
/// `deadline` bounds query work from the moment the request was accepted.
pub(crate) fn local(
    config: &WorkspaceConfig,
    cache: &Path,
    options: &Arguments,
    context: &RequestContext,
    session: &mut crate::semantic::SemanticSession,
    deadline: tp_core::query_deadline::QueryDeadline,
) -> Result<String> {
    let incoming_deadline = deadline;
    let local_deadline = local_execution_deadline(options, &incoming_deadline);
    session.set_no_daemon(options.no_daemon);
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    if let Some(rendered) = current_path_show(config, options, &local_deadline)? {
        return Ok(rendered);
    }
    let budget = OutputBudget::new(options.budget)?;
    if verb == "semantic" {
        return crate::cli::semantic_commands::workspace(
            config,
            options,
            context,
            &incoming_deadline,
        );
    }
    if verb == "ws" {
        return inspect(config, options, &budget, &local_deadline);
    }
    let results = WorkspaceResults::open(cache, &local_deadline)?;
    let page_budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    match verb {
        "more" => results.more(
            options
                .words
                .get(1)
                .with_coded_context(|| coded!(Usage, "more SET@OFFSET"))?,
            options.limit,
            &page_budget,
        ),
        "show" => owner_navigation::read(config, &results, options, &page_budget),
        "ctx" => owner_navigation::read(config, &results, options, &budget),
        "hist" | "since" | "diff" | "blame" => {
            owner_navigation::history(config, &results, options, &budget)
        }
        "index" | "init" | "status" | "doctor" | "hist-index" | "hist-status" | "session"
        | "audit" | "forget-logs" | "semantic-check" => {
            let member = selected_member(config, options)?;
            member.verify_identity()?;
            let mut local = member_options(options, &member, &local_deadline)?;
            local.budget = 1_000_000;
            let mut args = crate::cli::normalized_args(&local, &local.root);
            if options.wait {
                args.push("--wait".into());
            }
            // Direct: the router may be the caller still proxying this request.
            let mut value: Value = serde_json::from_str(&crate::cli::run_direct(
                &args,
                context,
                incoming_deadline.clone(),
            )?)?;
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
            bail!(coded!(InvalidCommand, "internal server command"))
        }
        "search" | "refs" | "map" => retrieval::search(
            config,
            cache,
            &results,
            options,
            context,
            session,
            local_deadline,
        ),
        _ => bail!(coded!(
            InvalidCommand,
            "unknown command {verb}; use search for queries"
        )),
    }
}
fn inspect(
    config: &WorkspaceConfig,
    options: &Arguments,
    budget: &OutputBudget,
    deadline: &QueryDeadline,
) -> Result<String> {
    let command = options.words.get(1).map(String::as_str).unwrap_or("show");
    ensure!(
        matches!(command, "show" | "status"),
        coded!(Usage, "ws show | ws status | ws discover PATH...")
    );
    let start = options.root.canonicalize()?;
    let roots = config.member_roots(&start)?;
    let mut members = Vec::with_capacity(roots.len());
    for member in &roots {
        ensure!(
            !deadline.expired(),
            coded!(TimedOut, "query deadline expired")
        );
        let cache = hashed_member_cache(&member.root, options.cache.as_deref())?;
        let mut value = json!({"member":member.name(),"root":encode_path(&member.root),"available":member.verify_identity().is_ok()});
        if let Some(label) = &member.worktree {
            value["worktree"] = label.clone().into();
            if command == "status"
                && let Some(seed) = crate::store::read_seed_outcome(&cache)
            {
                value["seed"] = seed;
            }
        }
        if command == "status"
            && member.verify_identity().is_ok()
            && cache.join("index.sqlite3").exists()
        {
            match crate::store::Store::open_read(&member.root, &cache, deadline.clone()) {
                Ok(store) => {
                    ensure!(
                        !deadline.expired(),
                        coded!(TimedOut, "query deadline expired")
                    );
                    value["generation"] = store.generation()?.into();
                    value["coverage"] = serde_json::to_value(store.coverage()?)?;
                }
                Err(error) if deadline.expired() => {
                    return Err(
                        error.with_coded_context(|| coded!(TimedOut, "query deadline expired"))
                    );
                }
                Err(error) if crate::store::is_index_warming(&error) => {
                    value["generation"] = 0.into();
                    value["state"] = "warming".into();
                }
                Err(_) => value["available"] = false.into(),
            }
        }
        members.push(value);
    }
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let home = config.home_root(&start)?;
    budget.render(&json!({"workspace":config.name,"config":encode_path(&config.path),"home":home.as_ref().map(MemberRoot::name),"members":members}))
}
pub fn discover_paths(paths: &[String], budget: usize) -> Result<String> {
    ensure!(!paths.is_empty(), coded!(Usage, "ws discover PATH..."));
    ensure!(
        paths.len() <= 32,
        coded!(InvalidWorkspace, "maximum 32 members")
    );
    let mut members = toml::map::Map::new();
    for path in paths {
        let root = workspace_config::resolve_path(Path::new(path), &std::env::current_dir()?)?
            .canonicalize()?;
        ensure!(
            root.is_dir(),
            coded!(InvalidWorkspace, "member must be a directory")
        );
        let name = root
            .file_name()
            .and_then(|s| s.to_str())
            .with_coded_context(|| coded!(InvalidWorkspace, "choose a member name explicitly"))?;
        ensure!(
            workspace_config::valid_name(name) && !members.contains_key(name),
            coded!(
                InvalidWorkspace,
                "member name is invalid or duplicated; write named members explicitly"
            )
        );
        let mut entry = toml::map::Map::new();
        entry.insert(
            "path".into(),
            toml::Value::String(
                root.to_str()
                    .with_coded_context(|| coded!(InvalidWorkspace, "config path is not UTF-8"))?
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
