//! One capability probe precedes board dispatch; local fallback requires the database pin.
use super::{
    BOARD_API, BoardConfig, BoardHost,
    board_grammar::{self, BoardCommand},
};
use crate::{cli::Arguments, daemon::deadline::QueryDeadline, diagnostics::RequestContext};
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

// Only the sized API number survives retries; source/configuration remain request-owned.
static PROBED_API: AtomicU64 = AtomicU64::new(0);

trait BoardGateway {
    fn request(&mut self, args: &[String], context: &RequestContext) -> Result<Option<String>>;
    fn ensure(&mut self) -> Result<()>;
}

struct SystemGateway;
impl BoardGateway for SystemGateway {
    fn request(&mut self, args: &[String], context: &RequestContext) -> Result<Option<String>> {
        crate::system::request(args, context)
    }
    fn ensure(&mut self) -> Result<()> {
        crate::system::ensure()
    }
}

pub(super) fn run(
    args: &[String],
    options: &Arguments,
    context: &RequestContext,
) -> Result<String> {
    let forwarded = super::prepare_client(args, options)?;
    let options = crate::cli::parse(&forwarded)?;
    let command = board_grammar::parse(&options, None)?;
    let encoded = crate::daemon::request_encoded_size(&forwarded, context)?;
    ensure!(
        encoded <= crate::daemon::MAX_DAEMON_REQUEST_BYTES,
        "invalid_body: encoded board request is {encoded} bytes; maximum is {} bytes; split the plan",
        crate::daemon::MAX_DAEMON_REQUEST_BYTES
    );
    run_prepared(
        &forwarded,
        &options,
        &command,
        context,
        &mut SystemGateway,
        &PROBED_API,
        &mut BoardConfig::load,
        crate::system::dir().as_deref(),
        &crate::system::spool_dir(),
    )
}

fn run_prepared(
    forwarded: &[String],
    options: &Arguments,
    command: &BoardCommand,
    context: &RequestContext,
    gateway: &mut dyn BoardGateway,
    api: &AtomicU64,
    load: &mut dyn FnMut() -> Result<BoardConfig>,
    runtime: Option<&Path>,
    spool: &Path,
) -> Result<String> {
    if options.no_daemon {
        return local_reply(options, command, context, load, runtime, spool);
    }
    let mut version = api.load(Ordering::Acquire);
    if version == 0 {
        let ping = ["system".to_owned(), "status".to_owned()];
        let mut status = match gateway.request(&ping, context) {
            Ok(status) => status,
            Err(error) => return failed_request(command, options, context, load, spool, error),
        };
        let mut recent = false;
        if status.is_none() {
            recent = runtime.is_some_and(|runtime| {
                crate::system::board_router_recently_unavailable(runtime, SystemTime::now())
            });
            if !recent {
                let _ = gateway.ensure();
                status = match gateway.request(&ping, context) {
                    Ok(status) => status,
                    Err(error) => {
                        return failed_request(command, options, context, load, spool, error);
                    }
                };
            }
        }
        let Some(status) = status else {
            if !recent {
                if let Some(runtime) = runtime {
                    let _ =
                        crate::system::mark_board_router_unavailable(runtime, SystemTime::now());
                }
            }
            return local_reply(options, command, context, load, runtime, spool);
        };
        let (reported, database) = parse_probe(&status)?;
        version = u64::from(reported) + 1;
        api.store(version, Ordering::Release);
        ensure!(
            reported == BOARD_API,
            "board_api_mismatch: router board API is {reported}; client expects {BOARD_API}; restart trufflepig-system.service"
        );
        if let Some(runtime) = runtime {
            let _ = crate::system::record_board_database(runtime, &database);
            crate::system::clear_board_router_unavailable(runtime);
        }
    }
    ensure!(
        version == u64::from(BOARD_API) + 1,
        "board_api_mismatch: router board API is {}; client expects {BOARD_API}; restart trufflepig-system.service",
        version - 1
    );
    match gateway.request(forwarded, context) {
        Ok(Some(reply)) => {
            if let Some(runtime) = runtime {
                crate::system::clear_board_router_unavailable(runtime);
            }
            Ok(reply)
        }
        Ok(None) => {
            if let Some(runtime) = runtime {
                let _ = crate::system::mark_board_router_unavailable(runtime, SystemTime::now());
            }
            local_reply(options, command, context, load, runtime, spool)
        }
        Err(error) => failed_request(command, options, context, load, spool, error),
    }
}

fn failed_request(
    command: &BoardCommand,
    options: &Arguments,
    context: &RequestContext,
    load: &mut dyn FnMut() -> Result<BoardConfig>,
    spool: &Path,
    error: anyhow::Error,
) -> Result<String> {
    if matches!(command, BoardCommand::Op(super::BoardOp::Feedback { .. })) {
        let config = load()?;
        super::unavailable_or_queue(command, options, context, &config, spool, error)
    } else {
        Err(error)
    }
}

fn parse_probe(status: &str) -> Result<(u32, PathBuf)> {
    let status: serde_json::Value = serde_json::from_str(status).context(
        "board_api_mismatch: router returned an invalid status; restart trufflepig-system.service",
    )?;
    let api = status
        .get("board_api")
        .and_then(serde_json::Value::as_u64)
        .and_then(|api| u32::try_from(api).ok())
        .context(
            "board_api_mismatch: router lacks board capability; restart trufflepig-system.service",
        )?;
    let database = status.get("board_db").and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .context("board_api_mismatch: router omitted its database path; restart trufflepig-system.service")?;
    ensure!(
        database.is_absolute(),
        "board_api_mismatch: router database path is not absolute"
    );
    Ok((api, database))
}

fn local_reply(
    options: &Arguments,
    command: &BoardCommand,
    context: &RequestContext,
    load: &mut dyn FnMut() -> Result<BoardConfig>,
    runtime: Option<&Path>,
    spool: &Path,
) -> Result<String> {
    let config = load()?;
    if let Some(runtime) = runtime {
        crate::system::validate_board_database(runtime, &config.db_path)?;
    }
    match BoardHost::with_config(config.clone()).run(options, context, QueryDeadline::start()) {
        Ok(reply) => Ok(reply),
        Err(error) => super::unavailable_or_queue(command, options, context, &config, spool, error),
    }
}

#[cfg(test)]
mod tests;
