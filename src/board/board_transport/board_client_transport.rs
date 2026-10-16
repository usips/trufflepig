//! One capability probe precedes board dispatch; local fallback requires the database pin.
use crate::board::{
    BOARD_API, BoardConfig, BoardHost,
    board_grammar::{self, BoardCommand},
};
use crate::{cli::Arguments, daemon::deadline::QueryDeadline, diagnostics::RequestContext};
use anyhow::{Context, Result, bail, ensure};
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

mod local_access;

/// Retains the router capability only while this transport serves requests.
#[derive(Default)]
pub(crate) struct BoardClientTransport {
    probed_api: Option<u32>,
}

trait BoardGateway {
    fn request(
        &mut self,
        args: &[String],
        context: &RequestContext,
        deadline: &QueryDeadline,
    ) -> Result<Option<String>>;
    fn ensure(&mut self, deadline: &QueryDeadline) -> Result<()>;

    /// Checks whether the known router socket is denied after normal routing
    /// has already tried both the socket and sandbox spool.
    fn probe_socket(&mut self, path: &Path) -> std::io::Result<()> {
        local_access::probe_socket(path)
    }
}

struct SystemGateway;
impl BoardGateway for SystemGateway {
    fn request(
        &mut self,
        args: &[String],
        context: &RequestContext,
        deadline: &QueryDeadline,
    ) -> Result<Option<String>> {
        crate::system::request_by(args, context, deadline.clone())
    }
    fn ensure(&mut self, deadline: &QueryDeadline) -> Result<()> {
        crate::system::ensure_by(deadline.clone())
    }
}

impl BoardClientTransport {
    pub(crate) fn run(
        &mut self,
        args: &[String],
        options: &Arguments,
        context: &RequestContext,
        deadline: QueryDeadline,
    ) -> Result<String> {
        let forwarded = crate::board::prepare_client(args, options)?;
        let options = crate::cli::parse(&forwarded)?;
        let command = board_grammar::parse(&options, None)?;
        let encoded = crate::daemon::request_encoded_size(&forwarded, context)?;
        ensure!(
            encoded <= crate::daemon::MAX_DAEMON_REQUEST_BYTES,
            "invalid_body: encoded board request is {encoded} bytes; maximum is {} bytes; split the plan",
            crate::daemon::MAX_DAEMON_REQUEST_BYTES
        );
        self.run_prepared(
            &forwarded,
            &options,
            &command,
            context,
            &mut SystemGateway,
            &mut BoardConfig::load,
            crate::system::dir().as_deref(),
            &crate::system::spool_dir(),
            &deadline,
        )
    }
    fn run_prepared(
        &mut self,
        forwarded: &[String],
        options: &Arguments,
        command: &BoardCommand,
        context: &RequestContext,
        gateway: &mut dyn BoardGateway,
        load: &mut dyn FnMut() -> Result<BoardConfig>,
        runtime: Option<&Path>,
        spool: &Path,
        deadline: &QueryDeadline,
    ) -> Result<String> {
        match self.run_prepared_inner(
            forwarded, options, command, context, gateway, load, runtime, spool, deadline,
        ) {
            Err(error)
                if matches!(
                    command,
                    BoardCommand::Op(crate::board::BoardOp::Feedback { .. })
                ) && error
                    .downcast_ref::<BoardRequestDeadlineExpired>()
                    .is_some() =>
            {
                failed_request(command, options, context, load, spool, deadline, error)
            }
            result => result,
        }
    }

    fn run_prepared_inner(
        &mut self,
        forwarded: &[String],
        options: &Arguments,
        command: &BoardCommand,
        context: &RequestContext,
        gateway: &mut dyn BoardGateway,
        load: &mut dyn FnMut() -> Result<BoardConfig>,
        runtime: Option<&Path>,
        spool: &Path,
        deadline: &QueryDeadline,
    ) -> Result<String> {
        if options.no_daemon {
            return local_reply(options, command, context, load, runtime, spool, deadline);
        }
        check_deadline(deadline)?;
        if self.probed_api.is_none() {
            let ping = ["system".to_owned(), "status".to_owned()];
            let mut status = match gateway.request(&ping, context, deadline) {
                Ok(status) => status,
                Err(error) => {
                    return failed_request(command, options, context, load, spool, deadline, error);
                }
            };
            let mut recent = false;
            if status.is_none() {
                check_deadline(deadline)?;
                let denied = local_access::denied_router_socket(gateway, runtime, load)?;
                check_deadline(deadline)?;
                if let Some(error) = denied {
                    return failed_request(command, options, context, load, spool, deadline, error);
                }
                recent = runtime.is_some_and(|runtime| {
                    crate::system::board_router_recently_unavailable(runtime, SystemTime::now())
                });
                if !recent {
                    check_deadline(deadline)?;
                    let _ = gateway.ensure(deadline);
                    check_deadline(deadline)?;
                    status = match gateway.request(&ping, context, deadline) {
                        Ok(status) => status,
                        Err(error) => {
                            return failed_request(
                                command, options, context, load, spool, deadline, error,
                            );
                        }
                    };
                }
            }
            let Some(status) = status else {
                check_deadline(deadline)?;
                if !recent {
                    if let Some(runtime) = runtime {
                        let _ = crate::system::mark_board_router_unavailable(
                            runtime,
                            SystemTime::now(),
                        );
                    }
                }
                return local_reply(options, command, context, load, runtime, spool, deadline);
            };
            let (reported, database) = parse_probe(&status)?;
            self.probed_api = Some(reported);
            ensure!(
                reported == BOARD_API,
                concat!(
                    "board_api_mismatch: router board API is {reported}; client expects {BOARD_API}; ",
                    "restart trufflepig-system.service"
                ),
                reported = reported,
                BOARD_API = BOARD_API
            );
            check_deadline(deadline)?;
            if let Some(runtime) = runtime {
                let _ = crate::system::record_board_database(runtime, &database);
                crate::system::clear_board_router_unavailable(runtime);
            }
        }
        ensure!(
            self.probed_api == Some(BOARD_API),
            "board_api_mismatch: router board API is {}; client expects {BOARD_API}; restart trufflepig-system.service",
            self.probed_api
                .expect("successful probe retains the router API")
        );
        check_deadline(deadline)?;
        match gateway.request(forwarded, context, deadline) {
            Ok(Some(reply)) => {
                if let Some(runtime) = runtime {
                    crate::system::clear_board_router_unavailable(runtime);
                }
                Ok(reply)
            }
            Ok(None) => {
                check_deadline(deadline)?;
                let denied = local_access::denied_router_socket(gateway, runtime, load)?;
                check_deadline(deadline)?;
                if let Some(error) = denied {
                    return failed_request(command, options, context, load, spool, deadline, error);
                }
                if let Some(runtime) = runtime {
                    let _ =
                        crate::system::mark_board_router_unavailable(runtime, SystemTime::now());
                }
                local_reply(options, command, context, load, runtime, spool, deadline)
            }
            Err(error) => failed_request(command, options, context, load, spool, deadline, error),
        }
    }
}

#[derive(Debug)]
struct BoardRequestDeadlineExpired;

impl std::fmt::Display for BoardRequestDeadlineExpired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("board request deadline expired")
    }
}

impl std::error::Error for BoardRequestDeadlineExpired {}

fn check_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        return Err(
            anyhow::Error::new(BoardRequestDeadlineExpired).context(format!(
                "{}: board request deadline expired",
                crate::daemon::deadline::TIMED_OUT
            )),
        );
    }
    Ok(())
}

fn failed_request(
    command: &BoardCommand,
    options: &Arguments,
    context: &RequestContext,
    load: &mut dyn FnMut() -> Result<BoardConfig>,
    spool: &Path,
    deadline: &QueryDeadline,
    error: anyhow::Error,
) -> Result<String> {
    if matches!(
        command,
        BoardCommand::Op(crate::board::BoardOp::Feedback { .. })
    ) {
        let config = load()?;
        crate::board::unavailable_or_queue(
            command, options, context, &config, spool, deadline, error,
        )
    } else {
        Err(crate::board::schema_upgrade_advice(error))
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
    let Some(path) = status.get("board_db").and_then(serde_json::Value::as_str) else {
        if let Some(detail) = status
            .get("board_error")
            .and_then(serde_json::Value::as_str)
        {
            bail!("board_unavailable: router board error: {detail}");
        }
        bail!(
            "board_api_mismatch: router omitted its database path; restart trufflepig-system.service"
        );
    };
    let database = PathBuf::from(path);
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
    deadline: &QueryDeadline,
) -> Result<String> {
    check_deadline(deadline)?;
    // A live router owns migration: feedback queues for it to import while
    // other operations run without migrating and refuse stale storage only.
    if runtime.is_some_and(|runtime| crate::daemon::running(runtime)) {
        if matches!(
            command,
            BoardCommand::Op(crate::board::BoardOp::Feedback { .. })
        ) {
            check_deadline(deadline)?;
            let config = load()?;
            check_deadline(deadline)?;
            return crate::board::unavailable_or_queue(
                command,
                options,
                context,
                &config,
                spool,
                deadline,
                anyhow::anyhow!("board_unavailable: router owns migration"),
            );
        }
        check_deadline(deadline)?;
        let config = load()?;
        check_deadline(deadline)?;
        if let Some(runtime) = runtime {
            crate::system::validate_board_database(runtime, &config.db_path)?;
        }
        check_deadline(deadline)?;
        refuse_stale_storage_beside_router(
            command, options, context, &config, runtime, spool, deadline,
        )?;
        return run_local(command, options, context, &config, runtime, spool, deadline);
    }
    check_deadline(deadline)?;
    let config = load()?;
    check_deadline(deadline)?;
    if let Some(runtime) = runtime {
        crate::system::validate_board_database(runtime, &config.db_path)?;
    }
    run_local(command, options, context, &config, runtime, spool, deadline)
}

/// Opens read-only beside a live router, so missing or stale storage refuses
/// without creating or migrating it; current files proceed to the local run.
fn refuse_stale_storage_beside_router(
    command: &BoardCommand,
    options: &Arguments,
    context: &RequestContext,
    config: &BoardConfig,
    runtime: Option<&Path>,
    spool: &Path,
    deadline: &QueryDeadline,
) -> Result<()> {
    use crate::board::local_board::LocalBoard;
    check_deadline(deadline)?;
    let timeout = deadline.cap(std::time::Duration::from_secs(5));
    check_deadline(deadline)?;
    let result = LocalBoard::open_read_with_timeout(config, timeout);
    check_deadline(deadline)?;
    match result {
        Ok(_) => Ok(()),
        Err(error)
            if !config.db_path.exists() || LocalBoard::needs_writable_initialization(&error) =>
        {
            bail!("board_unavailable: router owns migration")
        }
        Err(error) => {
            let error = anyhow::Error::from(error);
            if local_access::is_storage_access_failure(&error) {
                return Err(local_access::storage_access_advice(error, config, runtime));
            }
            crate::board::unavailable_or_queue(
                command, options, context, config, spool, deadline, error,
            )
            .map(|_| ())
        }
    }
}

fn run_local(
    command: &BoardCommand,
    options: &Arguments,
    context: &RequestContext,
    config: &BoardConfig,
    runtime: Option<&Path>,
    spool: &Path,
    deadline: &QueryDeadline,
) -> Result<String> {
    check_deadline(deadline)?;
    match BoardHost::with_config(config.clone()).run(options, context, deadline.clone()) {
        Ok(reply) => Ok(reply),
        Err(error)
            if local_access::is_storage_access_failure(&error)
                && !matches!(
                    command,
                    BoardCommand::Op(crate::board::BoardOp::Feedback { .. })
                ) =>
        {
            Err(local_access::storage_access_advice(error, config, runtime))
        }
        Err(error) => crate::board::unavailable_or_queue(
            command, options, context, config, spool, deadline, error,
        ),
    }
}

#[cfg(test)]
mod tests;
