//! Client dispatch with board capability state owned across retries.

use super::*;
use crate::board::board_transport::board_client_transport::BoardClientTransport;

#[derive(Default)]
pub(super) struct ClientCommand {
    board: BoardClientTransport,
}

impl ClientCommand {
    pub(super) fn run(
        &mut self,
        args: &[String],
        context: &crate::diagnostics::RequestContext,
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
            return semantic::serve_worker(&options);
        }
        if verb == "semantic" && options.words.get(1).is_some_and(|verb| verb == "worker") {
            return semantic::worker_command(&options);
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
                crate::board::board_grammar::parse(&options, None)?
            else {
                unreachable!("web grammar produces a web command");
            };
            return crate::board::board_web::link(target);
        }
        if matches!(verb, "board" | "feedback") {
            return self.board.run(args, &options, context);
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
                crate::system::request(&forwarded, context).and_then(|reply| match reply {
                    Some(reply) => Ok(Some(reply)),
                    None => {
                        let _ = crate::system::ensure();
                        crate::system::request(&forwarded, context)
                    }
                });
            match routed {
                Ok(Some(reply)) => {
                    return system_reply(&applied, &root, verb, config.as_ref(), reply);
                }
                // An unavailable owner or locally expired spool wait leaves direct
                // execution; other router errors remain answers.
                Err(error)
                    if !daemon::spool::is_local_spool_timeout(&error)
                        && !format!("{error:#}").contains("daemon_unavailable") =>
                {
                    return Err(error);
                }
                _ => {}
            }
        }
        direct(&options, context, CLIENT_REPLY_WAIT)
    }
}
