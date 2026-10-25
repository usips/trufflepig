//! Client dispatch with board capability state owned across retries.

use super::*;
use crate::board::board_transport::board_client_transport::BoardClientTransport;
use crate::coded_error::CodedContext;
use crate::coded_error::ErrorCode;

pub(super) fn request_pending(
    args: &[String],
    context: &crate::request_context::RequestContext,
    no_daemon: bool,
    system_dir: impl FnOnce() -> Option<PathBuf>,
    deadline: crate::query_deadline::QueryDeadline,
) -> Result<String> {
    if no_daemon {
        anyhow::bail!(coded!(
            InvalidOptions,
            "board pending requires an existing router"
        ));
    }
    let directory = system_dir()
        .with_coded_context(|| coded!(BoardUnavailable, "no running router for pending notices"))?;
    anyhow::ensure!(
        !deadline.expired(),
        coded!(TimedOut, "board pending request deadline expired")
    );
    crate::daemon_transport::request_by(&directory, args, context, deadline)?
        .with_coded_context(|| coded!(BoardUnavailable, "no running router for pending notices"))
}

#[derive(Default)]
pub(super) struct ClientCommand {
    board: BoardClientTransport,
}

impl ClientCommand {
    pub(super) fn run(
        &mut self,
        args: &[String],
        context: &crate::request_context::RequestContext,
        deadline: crate::query_deadline::QueryDeadline,
    ) -> Result<String> {
        if let Some(answer) = board_api_probe::probe_board_api(args) {
            return answer;
        }
        let options = parse(args)?;
        validate(&options)?;
        let verb = options
            .words
            .first()
            .map(String::as_str)
            .unwrap_or("status");
        if verb == "semantic-worker-serve" {
            return semantic_commands::serve_worker(&options);
        }
        if verb == "semantic" && options.words.get(1).is_some_and(|verb| verb == "worker") {
            return semantic_commands::worker_command(&options);
        }
        if verb == "ws" && options.words.get(1).is_some_and(|v| v == "discover") {
            return crate::workspace::discover_paths(&options.words[2..], options.budget);
        }
        if verb == "ws" && options.words.get(1).is_some_and(|v| v == "list") {
            return crate::workspace::list(&options);
        }
        if verb == "board-serve" {
            let address = options.board_listen_address();
            return crate::board::board_web::serve(address).map(|()| String::new());
        }
        if verb == "system-serve" {
            return crate::system::serve().map(|()| String::new());
        }
        if verb == "system" {
            return system_command(&options, context);
        }
        if verb == "board" && options.words.get(1).is_some_and(|word| word == "web") {
            let crate::board::board_grammar::BoardCommand::Web { target } =
                crate::board::board_grammar::parse(&options.board_arguments(), None)?
            else {
                unreachable!("web grammar produces a web command");
            };
            return crate::board::board_web::link(target);
        }
        if verb == "board" && options.words.get(1).is_some_and(|word| word == "pending") {
            let crate::board::board_grammar::BoardCommand::Op(_) =
                crate::board::board_grammar::parse(&options.board_arguments(), None)?
            else {
                unreachable!("pending grammar produces an operation");
            };
            return request_pending(
                args,
                context,
                options.no_daemon,
                crate::runtime_paths::system_dir,
                deadline,
            );
        }
        if matches!(verb, "board" | "feedback") {
            let forwarded = super::board_request_bridge::board_forwarded_args(&options)?;
            let replay = parse(&forwarded)?;
            return self.board.run(
                &forwarded,
                &replay.board_arguments(),
                context,
                crate::system::ensure_by,
                deadline,
            );
        }
        if options.no_cache {
            return super::direct_with_deadline(&options, context, deadline);
        }
        if system_routes(&options, verb)
            && let Ok(root) = options.root.canonicalize()
            && let Ok(config) = crate::workspace::resolve(&options)
        {
            let applied = match config.as_ref() {
                Some(config) => crate::workspace::apply_config(config, &options)?,
                None => options.clone(),
            };
            let mut forwarded = normalized_args(&applied, &root);
            if config.is_some()
                && options.wait
                && !(verb == "semantic" && options.words.get(1).is_some_and(|c| c == "prepare"))
            {
                forwarded.push("--wait".into());
            }
            let routed =
                crate::daemon_transport::router_request_by(&forwarded, context, deadline.clone())
                    .and_then(|reply| match reply {
                        Some(reply) => Ok(Some(reply)),
                        None if !deadline.expired() => {
                            let _ = crate::system::ensure_by(deadline.clone());
                            crate::daemon_transport::router_request_by(
                                &forwarded,
                                context,
                                deadline.clone(),
                            )
                        }
                        None => Ok(None),
                    });
            match routed {
                Ok(Some(reply)) => {
                    return system_reply(&applied, &root, verb, config.as_ref(), reply);
                }
                Err(error) => {
                    if let Some(fallback_options) = index_cache_fallback_options(args, &error) {
                        return super::direct_with_deadline(&fallback_options, context, deadline);
                    }
                    if !leaves_router_for_direct_execution(&error) {
                        return Err(error);
                    }
                }
                _ => {}
            }
        }
        super::direct_with_deadline(&options, context, deadline)
    }

    pub(super) fn run_after_busy_exhaustion(
        &mut self,
        args: &[String],
        context: &crate::request_context::RequestContext,
        deadline: crate::query_deadline::QueryDeadline,
    ) -> Option<Result<String>> {
        let options = local_path_show_fallback_options(args)?;
        Some(super::direct_with_deadline(&options, context, deadline))
    }
}

/// Whether a failed read can fall back to the local current-path reader.
pub(super) fn has_busy_fallback(args: &[String]) -> bool {
    local_path_show_fallback_options(args).is_some()
}

fn local_path_show_fallback_options(args: &[String]) -> Option<Arguments> {
    let mut options = parse(args).ok()?;
    validate(&options).ok()?;
    if options.words.first().is_none_or(|verb| verb != "show")
        || !options
            .words
            .get(1)
            .is_some_and(|target| crate::source::acquisition::is_current_path_target(target))
    {
        return None;
    }
    options.no_daemon = true;
    Some(options)
}

/// An unavailable owner, named in any layer or in relayed text, or a locally
/// expired spool wait leaves the router for direct execution; other router
/// errors remain answers.
fn leaves_router_for_direct_execution(error: &anyhow::Error) -> bool {
    crate::daemon_transport::spool::is_local_spool_timeout(error)
        || ErrorCode::DaemonUnavailable.is_named_in(error)
}

/// A cache refusal in any layer admits the local current-path reader, unless
/// any layer reports a timeout.
fn index_cache_fallback_options(args: &[String], error: &anyhow::Error) -> Option<Arguments> {
    let timed_out = crate::daemon_transport::DaemonReplyTimeout::from_error(error).is_some()
        || ErrorCode::TimedOut.is_anywhere_in(error)
        || ErrorCode::DaemonReplyTimeout.is_anywhere_in(error);
    if timed_out || !ErrorCode::IndexCacheUnavailable.is_anywhere_in(error) {
        return None;
    }
    local_path_show_fallback_options(args)
}

#[cfg(test)]
mod tests;
