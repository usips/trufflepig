//! Durable plan coordination through the per-machine router and a typed backend.
//! The client reads bodies and normalizes text before socket or spool transport.
//! Only an absent router permits local fallback; ambiguous replies never replay writes.
pub mod board_actor;
pub mod board_backend;
pub mod board_config;
pub mod board_grammar;
pub mod board_ids;
pub mod board_protocol;
pub mod board_render;
pub mod board_vocabulary;
pub mod commit_ingest;
pub mod commit_trailers;
pub mod feedback_outbox;
pub mod local_board;
pub mod repo_identity;
pub mod review_packet;
#[cfg(test)]
mod tests;

pub use board_backend::{BoardBackend, BoardHost};
pub use board_config::BoardConfig;
pub use board_protocol::{BOARD_API, BoardError, BoardOp, BoardReply, BoardRequest};

use crate::{cli::Arguments, daemon::deadline::QueryDeadline, diagnostics::RequestContext};
use anyhow::{Context, Result, bail, ensure};
use board_grammar::BoardCommand;
use std::io::Read;

/// Reads client-owned body files, then uses the system gateway or an absent-router fallback.
pub fn run_client(
    args: &[String],
    options: &Arguments,
    context: &RequestContext,
) -> Result<String> {
    let forwarded = prepare_client(args, options)?;
    let forwarded_options = crate::cli::parse(&forwarded)?;
    let command = board_grammar::parse(&forwarded_options, None)?;
    let encoded = crate::daemon::request_encoded_size(&forwarded, context)?;
    ensure!(
        encoded <= crate::daemon::MAX_DAEMON_REQUEST_BYTES,
        "invalid_body: encoded board request is {encoded} bytes; maximum is {} bytes; split the plan",
        crate::daemon::MAX_DAEMON_REQUEST_BYTES
    );
    let first = crate::system::request(&forwarded, context);
    let answered = match first {
        Ok(None) => {
            let _ = crate::system::ensure();
            crate::system::request(&forwarded, context)
        }
        answered => answered,
    };
    match answered {
        Ok(Some(reply)) => Ok(reply),
        Ok(None) => {
            let host = BoardHost::default();
            match host.run(&forwarded_options, context, QueryDeadline::start()) {
                Ok(reply) => Ok(reply),
                Err(error) => unavailable_or_queue(&command, &forwarded_options, context, error),
            }
        }
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("unknown_command:")
                && (message.contains("board") || message.contains("feedback"))
            {
                return Err(error.context("restart trufflepig-system.service to enable the board"));
            }
            // A failed reply may follow a successful write. Only feedback can queue
            // the same permanent import key safely; ordinary operations keep the error.
            if matches!(&command, BoardCommand::Op(BoardOp::Feedback { .. })) {
                unavailable_or_queue(&command, &forwarded_options, context, error)
            } else {
                Err(error)
            }
        }
    }
}

/// Captures stdin/file bodies and feedback identity once, including across retries.
pub(crate) fn prepare_client(args: &[String], options: &Arguments) -> Result<Vec<String>> {
    let body = read_body(options)?;
    board_grammar::normalize_args(args, options, body.as_deref())
}

fn read_body(options: &Arguments) -> Result<Option<String>> {
    let Some(path) = &options.board.body else {
        return Ok(None);
    };
    let maximum = board_vocabulary::PLAN_TEXT_LIMIT;
    let mut text = String::new();
    if path.as_os_str() == "-" {
        std::io::stdin()
            .lock()
            .take((maximum + 1) as u64)
            .read_to_string(&mut text)
            .context("invalid_body: cannot read stdin as UTF-8")?;
    } else {
        std::fs::File::open(path)
            .context("invalid_body: cannot open body file")?
            .take((maximum + 1) as u64)
            .read_to_string(&mut text)
            .context("invalid_body: cannot read body file as UTF-8")?;
    }
    ensure!(
        text.len() <= maximum,
        "invalid_body: body exceeds {maximum} bytes"
    );
    Ok(Some(text))
}

fn unavailable_or_queue(
    command: &BoardCommand,
    options: &Arguments,
    context: &RequestContext,
    error: anyhow::Error,
) -> Result<String> {
    let message = format!("{error:#}");
    if domain_answer(&message) {
        return Err(error);
    }
    if let BoardCommand::Op(op @ BoardOp::Feedback { .. }) = command {
        let config = BoardConfig::load()?;
        let actor = config.actor(context.client.as_deref(), context.session.as_deref())?;
        let mut op = op.clone();
        if let BoardOp::Feedback { metadata, plan, .. } = &mut op {
            if let Ok(Some(registration)) = repo_identity::register_repository(
                &options.root,
                &actor.host,
                *plan,
                std::time::Duration::from_secs(2),
            ) {
                metadata.repo_key = Some(registration.repo_key);
            }
            enrich_feedback_cwd(metadata, &options.root, std::time::Duration::from_secs(2));
        }
        let mut request = BoardRequest::new(actor, op);
        if options.board.agent_model.is_some() || options.board.agent_effort.is_some() {
            request.claims = Some(board_protocol::AgentClaims {
                model: options.board.agent_model.clone(),
                effort: options.board.agent_effort.clone(),
            });
        }
        let reply = feedback_outbox::queue(&crate::system::spool_dir(), &request)?;
        let budget =
            crate::output::OutputBudget::new(options.budget)?.with_format(options.output_format());
        return Ok(board_render::render_reply(&reply, &budget)?.text);
    }
    bail!("board_unavailable: {message}; run trufflepig system ensure")
}

fn domain_answer(message: &str) -> bool {
    message.split(':').any(|segment| {
        let code = segment.trim();
        code.starts_with("invalid_")
            || matches!(
                code,
                "board_remote_unsupported"
                    | "board_api_mismatch"
                    | "usage"
                    | "stale_revision"
                    | "claim_conflict"
                    | "budget_too_small"
                    | "unknown_command"
            )
    })
}

pub(crate) fn enrich_feedback_cwd(
    metadata: &mut board_protocol::FeedbackMetadata,
    directory: &std::path::Path,
    timeout: std::time::Duration,
) {
    if let Ok(Some(root)) = repo_identity::repository_root(directory, timeout) {
        if let Ok(directory) = directory.canonicalize() {
            if let Ok(relative) = directory.strip_prefix(root) {
                metadata.cwd = if relative.as_os_str().is_empty() {
                    ".".to_owned()
                } else {
                    relative.to_string_lossy().into_owned()
                };
            }
        }
    }
}

#[cfg(test)]
mod client_tests {
    use super::*;

    #[test]
    fn captured_body_survives_file_changes_between_retries() {
        let scratch =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/board-client-tests");
        std::fs::create_dir_all(&scratch).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("capture-")
            .tempdir_in(scratch)
            .unwrap();
        let body_path = directory.path().join("plan.md");
        std::fs::write(&body_path, "- captured plan\n").unwrap();
        let args = vec![
            "--body".into(),
            body_path.to_string_lossy().into_owned(),
            "board".into(),
            "new".into(),
            "Trial".into(),
        ];
        let options = crate::cli::parse(&args).unwrap();
        let prepared = prepare_client(&args, &options).unwrap();
        std::fs::write(&body_path, "changed before replay").unwrap();
        let prepared_options = crate::cli::parse(&prepared).unwrap();
        let replay = prepare_client(&prepared, &prepared_options).unwrap();
        let replay_options = crate::cli::parse(&replay).unwrap();
        let BoardCommand::Op(BoardOp::New { body, .. }) =
            board_grammar::parse(&replay_options, None).unwrap()
        else {
            panic!("expected a new plan");
        };
        assert_eq!(body.as_str(), "- captured plan\n");
        assert!(replay_options.board.body.is_none());
    }

    #[test]
    fn feedback_domain_answers_are_not_queued() {
        for code in [
            "board_remote_unsupported",
            "board_api_mismatch",
            "invalid_reference",
            "usage",
            "stale_revision",
            "claim_conflict",
            "budget_too_small",
        ] {
            for prefix in ["", "daemon: ", "daemon: daemon: "] {
                assert!(domain_answer(&format!("{prefix}{code}: failure")));
            }
        }
        for message in [
            "board_unavailable: permission denied",
            "daemon: read frame: broken pipe",
            "database is locked",
        ] {
            assert!(!domain_answer(message));
        }
    }
}
