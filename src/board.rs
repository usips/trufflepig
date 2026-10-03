//! Durable plan coordination through the per-machine router and a typed backend.
//! The client reads bodies and normalizes text before socket or spool transport.
//! Only an absent router permits local fallback; ambiguous replies never replay writes.
pub mod board_actor;
pub mod board_backend;
mod board_client_transport;
pub mod board_config;
pub mod board_grammar;
pub mod board_ids;
pub mod board_protocol;
pub mod board_render;
#[cfg(test)]
pub(crate) mod board_test_support;
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

use crate::{cli::Arguments, diagnostics::RequestContext};
use anyhow::{Context, Result, bail, ensure};
use board_grammar::BoardCommand;
use std::io::Read;

/// Reads client-owned body files, then uses the system gateway or an absent-router fallback.
pub fn run_client(
    args: &[String],
    options: &Arguments,
    context: &RequestContext,
) -> Result<String> {
    board_client_transport::run(args, options, context)
}

/// Captures stdin/file bodies and feedback identity once, including across retries.
pub(crate) fn prepare_client(args: &[String], options: &Arguments) -> Result<Vec<String>> {
    board_grammar::validate_before_body(options)?;
    let body = read_body(options)?;
    board_grammar::normalize_args(args, options, body.as_deref())
}

fn read_body(options: &Arguments) -> Result<Option<String>> {
    let Some(path) = &options.board.body else {
        return Ok(None);
    };
    let maximum = board_grammar::body_limit(options);
    let mut bytes = Vec::with_capacity(maximum.min(4096));
    if path.as_os_str() == "-" {
        std::io::stdin()
            .lock()
            .take((maximum + 1) as u64)
            .read_to_end(&mut bytes)
            .context("invalid_body: cannot read stdin")?;
    } else {
        std::fs::File::open(path)
            .context("invalid_body: cannot open body file")?
            .take((maximum + 1) as u64)
            .read_to_end(&mut bytes)
            .context("invalid_body: cannot read body file")?;
    }
    ensure!(
        bytes.len() <= maximum,
        "invalid_body: body exceeds {maximum} bytes"
    );
    let text = String::from_utf8(bytes).context("invalid_body: body is not UTF-8")?;
    Ok(Some(text))
}

fn unavailable_or_queue(
    command: &BoardCommand,
    options: &Arguments,
    context: &RequestContext,
    config: &BoardConfig,
    spool: &std::path::Path,
    error: anyhow::Error,
) -> Result<String> {
    let message = format!("{error:#}");
    if domain_error(&error) {
        return Err(error);
    }
    if let BoardCommand::Op(op @ BoardOp::Feedback { .. }) = command {
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
        let reply = feedback_outbox::queue(spool, &request)?;
        let budget =
            crate::output::OutputBudget::new(options.budget)?.with_format(options.output_format());
        return Ok(board_render::render_reply(&reply, &budget)?.text);
    }
    bail!("board_unavailable: {message}; run trufflepig system ensure")
}

fn domain_error(error: &anyhow::Error) -> bool {
    if let Some(code) = board_protocol::BoardErrorCode::from_error(error) {
        return code.is_domain_answer();
    }
    if error
        .chain()
        .any(|cause| cause.downcast_ref::<std::io::Error>().is_some())
    {
        return false;
    }
    error.chain().any(|cause| {
        board_protocol::leading_error_code(&cause.to_string()).is_some_and(|(code, _)| {
            code.starts_with("invalid_") || matches!(code, "budget_too_small" | "unknown_command")
        })
    })
}

#[cfg(test)]
fn domain_answer(message: &str) -> bool {
    domain_error(&anyhow::anyhow!(message.to_owned()))
}

pub(crate) fn enrich_feedback_cwd(
    metadata: &mut board_protocol::FeedbackMetadata,
    directory: &std::path::Path,
    timeout: std::time::Duration,
) {
    metadata.cwd.clear();
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
        let directory = crate::board::board_test_support::scratch("board-transport-");
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
    fn body_preflight_precedes_file_reads_and_byte_limits_precede_utf8() {
        let args: Vec<String> = [
            "--body",
            "missing-body-file",
            "board",
            "propose",
            "P0@1",
            "summary",
        ]
        .map(str::to_owned)
        .into();
        let options = crate::cli::parse(&args).unwrap();
        let error = prepare_client(&args, &options).unwrap_err();
        assert!(
            error.to_string().starts_with("invalid_reference:"),
            "{error:#}"
        );
        let directory = crate::board::board_test_support::scratch("board-runtime-");
        let path = directory.path().join("oversized-feedback");
        std::fs::write(&path, vec![0xff; 4097]).unwrap();
        let args = vec![
            "--body".into(),
            path.to_string_lossy().into_owned(),
            "feedback".into(),
            "blocked".into(),
            "summary".into(),
        ];
        let options = crate::cli::parse(&args).unwrap();
        let error = prepare_client(&args, &options).unwrap_err();
        assert!(
            error.to_string().contains("body exceeds 4096 bytes"),
            "{error:#}"
        );
    }

    #[test]
    fn typed_transient_cause_wins_over_domain_looking_context() {
        let error = anyhow::Error::new(board_protocol::BoardError::new(
            board_protocol::BoardErrorCode::BoardUnavailable,
            "temporarily offline",
        ))
        .context("invalid_body: context is diagnostic prose");
        assert!(!domain_error(&error));
        assert!(!domain_error(
            &anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
                .context("budget_too_small: diagnostic context")
        ));
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
            "read frame failed: invalid_body: incidental prose",
            "write failed: invalid_options: diagnostic mention",
            "unknown_code: stale_revision: detail",
            "read frame failed: invalid_body: incidental prose",
            "write failed: invalid_options: diagnostic mention",
            "unknown_code: stale_revision: detail",
            "board_unavailable: permission denied",
            "daemon: read frame: broken pipe",
            "database is locked",
        ] {
            assert!(!domain_answer(message));
        }
    }
}
