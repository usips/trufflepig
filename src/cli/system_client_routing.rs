//! System command routing and reply waits on the client.

use super::*;

/// Routes a verb through the system daemon unless it must run locally.
pub(super) fn system_routes(options: &Arguments, verb: &str) -> bool {
    if options.no_daemon
        || matches!(
            verb,
            "serve"
                | "history-serve"
                | "workspace-serve"
                | "system-serve"
                | "system"
                | "ws"
                | "stop"
                | "index"
                | "init"
                | "semantic-check"
        )
    {
        return false;
    }
    !(verb == "semantic"
        && options
            .words
            .get(1)
            .is_some_and(|command| command == "status"))
}

/// Handles the `system` verb against the per-user routing daemon.
pub(super) fn system_command(
    options: &Arguments,
    context: &crate::diagnostics::RequestContext,
) -> Result<String> {
    let render = |json: &str| -> Result<String> {
        OutputBudget::new(options.budget)?.render(&serde_json::from_str::<serde_json::Value>(json)?)
    };
    match options.words.get(1).map(String::as_str) {
        Some("ensure") => {
            crate::system::ensure()?;
            render("{\"status\":\"ok\"}")
        }
        Some("stop") => render(&crate::system::stop()?),
        Some("dir") => match crate::system::dir() {
            Some(dir) => Ok(format!("{}\n", dir.display())),
            None => anyhow::bail!("system_unavailable: no runtime dir"),
        },
        Some("prune") => {
            let evicted = crate::system::sweep::sweep(
                &cache_base()?,
                std::time::SystemTime::now(),
                crate::system::sweep::SWEEP_GRACE,
            );
            render(&serde_json::to_string(
                &serde_json::json!({ "evicted": evicted }),
            )?)
        }
        None | Some("status") => {
            let ping = vec!["system".to_owned(), "status".to_owned()];
            let status = if crate::system::request(&ping, context)
                .ok()
                .flatten()
                .is_some()
            {
                "ok"
            } else {
                "not_running"
            };
            render(&format!("{{\"status\":\"{status}\"}}"))
        }
        Some(other) => {
            anyhow::bail!(
                "usage: system status | system ensure | system stop | system prune | system dir (got {other})"
            )
        }
    }
}

/// Applies to a system-routed reply the waits a local route would have run.
pub(super) fn system_reply(
    options: &Arguments,
    root: &Path,
    verb: &str,
    config: Option<&crate::workspace::config::WorkspaceConfig>,
    reply: String,
) -> Result<String> {
    if let Some(config) = config {
        if options.wait
            && verb == "semantic"
            && options
                .words
                .get(1)
                .is_some_and(|command| command == "prepare")
        {
            return semantic::wait_for_workspace(config, options, reply);
        }
        return Ok(reply);
    }
    let reply = wait_for_history(root, options, reply)?;
    if verb == "semantic" {
        let cache = cache_path(root, options.cache.as_deref())?;
        semantic::wait_for_schedule(root, &cache, options, reply)
    } else {
        Ok(reply)
    }
}
