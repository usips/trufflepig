//! One capability probe precedes board dispatch; local fallback requires the database pin.
use crate::board_grammar::BoardArguments;
use crate::{
    BOARD_API, BoardConfig, BoardHost, BoardRequest, BoardRequestKey,
    board_grammar::{self, BoardCommand},
    board_protocol::{AgentClaims, BoardOp},
};
use anyhow::{Result, bail, ensure};
use std::{
    error::Error as StdError,
    fmt,
    path::{Path, PathBuf},
    time::SystemTime,
};
use tp_core::coded_error::{CodedContext, CodedError, ErrorCode};
use tp_core::{query_deadline::QueryDeadline, request_context::RequestContext};

mod local_access;

/// Retains the router capability only while this transport serves requests.
#[derive(Default)]
pub struct BoardClientTransport {
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

/// Routes through the system router; `ensure_router` starts it when absent.
struct SystemGateway {
    ensure_router: fn(QueryDeadline) -> Result<()>,
}
impl BoardGateway for SystemGateway {
    fn request(
        &mut self,
        args: &[String],
        context: &RequestContext,
        deadline: &QueryDeadline,
    ) -> Result<Option<String>> {
        tp_core::daemon_transport::router_request_by(args, context, deadline.clone())
    }
    fn ensure(&mut self, deadline: &QueryDeadline) -> Result<()> {
        (self.ensure_router)(deadline.clone())
    }
}

impl BoardClientTransport {
    /// Sends a prepared request: `forwarded` is the router argv for `options`,
    /// and `ensure_router` starts the system router when none answers.
    pub fn run(
        &mut self,
        forwarded: &[String],
        options: &BoardArguments,
        context: &RequestContext,
        ensure_router: fn(QueryDeadline) -> Result<()>,
        deadline: QueryDeadline,
    ) -> Result<String> {
        let command = board_grammar::parse(options, None)?;
        let encoded = tp_core::daemon_transport::request_encoded_size(forwarded, context)?;
        ensure!(
            encoded <= tp_core::daemon_transport::MAX_DAEMON_REQUEST_BYTES,
            coded!(
                InvalidBody,
                "encoded board request is {encoded} bytes; maximum is {} bytes; split the plan",
                tp_core::daemon_transport::MAX_DAEMON_REQUEST_BYTES
            )
        );
        self.run_prepared(
            forwarded,
            options,
            &command,
            context,
            &mut SystemGateway { ensure_router },
            &mut BoardConfig::load,
            tp_core::runtime_paths::system_dir().as_deref(),
            &tp_core::runtime_paths::spool_dir(),
            &deadline,
        )
    }
    fn run_prepared(
        &mut self,
        forwarded: &[String],
        options: &BoardArguments,
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
                if matches!(command, BoardCommand::Op(crate::BoardOp::Feedback { .. }))
                    && error
                        .chain()
                        .any(|cause| cause.is::<BoardRequestDeadlineExpired>()) =>
            {
                failed_request(
                    command, options, context, load, spool, deadline, false, error,
                )
            }
            result => result,
        }
    }

    fn run_prepared_inner(
        &mut self,
        forwarded: &[String],
        options: &BoardArguments,
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
                    return failed_request(
                        command, options, context, load, spool, deadline, false, error,
                    );
                }
            };
            let mut recent = false;
            if status.is_none() {
                check_deadline(deadline)?;
                let denied = local_access::denied_router_socket(gateway, runtime, load)?;
                check_deadline(deadline)?;
                if let Some(error) = denied {
                    return failed_request(
                        command, options, context, load, spool, deadline, false, error,
                    );
                }
                recent = runtime.is_some_and(|runtime| {
                    crate::board_runtime_markers::board_router_recently_unavailable(
                        runtime,
                        SystemTime::now(),
                    )
                });
                if !recent {
                    check_deadline(deadline)?;
                    let _ = gateway.ensure(deadline);
                    check_deadline(deadline)?;
                    status = match gateway.request(&ping, context, deadline) {
                        Ok(status) => status,
                        Err(error) => {
                            return failed_request(
                                command, options, context, load, spool, deadline, false, error,
                            );
                        }
                    };
                }
            }
            let Some(status) = status else {
                check_deadline(deadline)?;
                if !recent {
                    if let Some(runtime) = runtime {
                        let _ = crate::board_runtime_markers::mark_board_router_unavailable(
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
                coded!(
                    BoardApiMismatch,
                    concat!(
                        "router board API is {reported}; client expects {BOARD_API}; ",
                        "restart trufflepig-system.service"
                    ),
                    reported = reported,
                    BOARD_API = BOARD_API
                )
            );
            check_deadline(deadline)?;
            if let Some(runtime) = runtime {
                let _ = crate::board_runtime_markers::record_board_database(runtime, &database);
                crate::board_runtime_markers::clear_board_router_unavailable(runtime);
            }
        }
        ensure!(
            self.probed_api == Some(BOARD_API),
            coded!(
                BoardApiMismatch,
                "router board API is {}; client expects {BOARD_API}; restart trufflepig-system.service",
                self.probed_api
                    .expect("successful probe retains the router API")
            )
        );
        check_deadline(deadline)?;
        match gateway.request(forwarded, context, deadline) {
            Ok(Some(reply)) => {
                if let Some(runtime) = runtime {
                    crate::board_runtime_markers::clear_board_router_unavailable(runtime);
                }
                Ok(reply)
            }
            Ok(None) => {
                check_deadline(deadline)?;
                let denied = local_access::denied_router_socket(gateway, runtime, load)?;
                check_deadline(deadline)?;
                if let Some(error) = denied {
                    return failed_request(
                        command, options, context, load, spool, deadline, false, error,
                    );
                }
                if let Some(runtime) = runtime {
                    let _ = crate::board_runtime_markers::mark_board_router_unavailable(
                        runtime,
                        SystemTime::now(),
                    );
                }
                local_reply(options, command, context, load, runtime, spool, deadline)
            }
            Err(error) => failed_request(
                command, options, context, load, spool, deadline, true, error,
            ),
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
        return Err(anyhow::Error::new(BoardRequestDeadlineExpired)
            .with_coded_context(|| coded!(TimedOut, "board request deadline expired")));
    }
    Ok(())
}

fn failed_request(
    command: &BoardCommand,
    options: &BoardArguments,
    context: &RequestContext,
    load: &mut dyn FnMut() -> Result<BoardConfig>,
    spool: &Path,
    deadline: &QueryDeadline,
    board_request_attempted: bool,
    error: anyhow::Error,
) -> Result<String> {
    if let Some(timeout) = tp_core::daemon_transport::DaemonReplyTimeout::from_error(&error)
        && matches!(command, BoardCommand::Op(op) if !op.is_read_only() && !matches!(op, BoardOp::Feedback { .. }))
    {
        if !board_request_attempted {
            return Err(BoardPreflightTimeout {
                timeout: timeout.clone(),
            }
            .into());
        }
        let receipt_key = request_receipt_key(command, options, context, load)
            .ok()
            .flatten();
        return Err(AmbiguousBoardWriteTimeout {
            timeout: timeout.clone(),
            receipt_key,
        }
        .into());
    }
    if board_request_attempted
        && let Some(retry_after_ms) = board_admission_retry_after_ms(command, &error)
    {
        return Err(BoardAdmissionBusyAdvice {
            error,
            retry_after_ms,
        }
        .into());
    }
    if matches!(command, BoardCommand::Op(crate::BoardOp::Feedback { .. })) {
        let config = load()?;
        crate::unavailable_or_queue(command, options, context, &config, spool, deadline, error)
    } else {
        Err(crate::schema_upgrade_advice(error))
    }
}

/// Returns a retry hint only for a typed refusal from the router Board queue.
fn board_admission_retry_after_ms(command: &BoardCommand, error: &anyhow::Error) -> Option<u64> {
    let BoardCommand::Op(op) = command else {
        return None;
    };
    if op.is_read_only() || matches!(op, BoardOp::Feedback { .. }) {
        return None;
    }

    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<CodedError>())
        .filter(|coded| coded.code() == ErrorCode::DaemonBusy)
        .find_map(|coded| router_board_queue_full_retry_after_ms(coded.message()))
}

/// Admission refusals cross the daemon boundary as coded text. Require every
/// field emitted by `AdmissionRefusal` before treating `daemon_busy` as proof
/// that a Board mutation was not dispatched.
fn router_board_queue_full_retry_after_ms(message: &str) -> Option<u64> {
    let field = |name| unique_admission_field(message, name);
    if field("pool")? != "router-board"
        || field("cause")? != "queue_full"
        || field("hint")? != "retry_with_bounded_backoff"
    {
        return None;
    }

    let depth = field("queue_depth")?.parse::<usize>().ok()?;
    let capacity = field("queue_capacity")?.parse::<usize>().ok()?;
    if capacity == 0 || depth != capacity {
        return None;
    }

    let retry_after_ms = field("retry_after_ms")?.parse::<u64>().ok()?;
    (retry_after_ms > 0).then_some(retry_after_ms)
}

fn unique_admission_field<'a>(message: &'a str, name: &str) -> Option<&'a str> {
    let mut values = message.split_ascii_whitespace().filter_map(|token| {
        let (field, value) = token.split_once('=')?;
        (field == name).then_some(value)
    });
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

#[derive(Debug)]
struct BoardAdmissionBusyAdvice {
    error: anyhow::Error,
    retry_after_ms: u64,
}

impl fmt::Display for BoardAdmissionBusyAdvice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            concat!(
                "{}; board_write_outcome=not_dispatched; this router queue refusal proves the ",
                "Board write was not applied; the bounded retry honors at least {} ms backoff; ",
                "if it still fails, a manual --no-daemon retry requires the same command/body and ",
                "actor/session, the exact router-published Board database, and database, ",
                "parent-directory, SQLite WAL/SHM write access within the filesystem sandbox; ",
                "no direct fallback occurs automatically"
            ),
            self.error, self.retry_after_ms
        )
    }
}

impl StdError for BoardAdmissionBusyAdvice {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.error.as_ref())
    }
}

/// How long a caller waits before resending a write whose preflight timed out.
const PREFLIGHT_RETRY_AFTER_MS: u32 = 2000;

#[derive(Debug)]
struct BoardPreflightTimeout {
    timeout: tp_core::daemon_transport::DaemonReplyTimeout,
}

impl fmt::Display for BoardPreflightTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            concat!(
                "{}; board_write_outcome=not_dispatched board_preflight=system_status ",
                "retry_after_ms={}; Board write was not sent; resend the same command after ",
                "retry_after_ms; if the router stays unresponsive, verify the same actor/session ",
                "and exact Board database before retrying with --no-daemon; local writes require ",
                "filesystem write access to the database and its parent directory (SQLite WAL/SHM ",
                "files), including sandbox permission when required outside the workspace"
            ),
            self.timeout, PREFLIGHT_RETRY_AFTER_MS
        )
    }
}

impl StdError for BoardPreflightTimeout {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.timeout)
    }
}

#[derive(Debug)]
struct AmbiguousBoardWriteTimeout {
    timeout: tp_core::daemon_transport::DaemonReplyTimeout,
    receipt_key: Option<BoardRequestKey>,
}

impl fmt::Display for AmbiguousBoardWriteTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}; board_write_outcome=unknown", self.timeout)?;
        if let Some(key) = self.receipt_key {
            write!(
                formatter,
                " request_key={key}; inspect with `board receipt {key}`"
            )?;
        } else {
            formatter.write_str("; no BoardChange receipt key is available for this operation")?;
        }
        formatter.write_str("; do not automatically replay this mutation")
    }
}

impl StdError for AmbiguousBoardWriteTimeout {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.timeout)
    }
}

fn request_receipt_key(
    command: &BoardCommand,
    options: &BoardArguments,
    context: &RequestContext,
    load: &mut dyn FnMut() -> Result<BoardConfig>,
) -> Result<Option<BoardRequestKey>> {
    let BoardCommand::Op(op) = command else {
        return Ok(None);
    };
    if !op.has_change_receipt() {
        return Ok(None);
    }
    let config = load()?;
    let actor = config.actor(context.client.as_deref(), context.session.as_deref())?;
    let mut request = BoardRequest::new(actor, op.clone());
    request.origin = context.origin.clone();
    if options.board.agent_model.is_some() || options.board.agent_effort.is_some() {
        request.claims = Some(AgentClaims {
            model: options.board.agent_model.clone(),
            effort: options.board.agent_effort.clone(),
        });
    }
    Ok(Some(request.dedupe_key()?))
}

fn parse_probe(status: &str) -> Result<(u32, PathBuf)> {
    let status: serde_json::Value = serde_json::from_str(status).with_coded_context(|| {
        coded!(
            BoardApiMismatch,
            "router returned an invalid status; restart trufflepig-system.service"
        )
    })?;
    let api = status
        .get("board_api")
        .and_then(serde_json::Value::as_u64)
        .and_then(|api| u32::try_from(api).ok())
        .with_coded_context(|| {
            coded!(
                BoardApiMismatch,
                "router lacks board capability; restart trufflepig-system.service"
            )
        })?;
    let Some(path) = status.get("board_db").and_then(serde_json::Value::as_str) else {
        if let Some(detail) = status
            .get("board_error")
            .and_then(serde_json::Value::as_str)
        {
            bail!(coded!(BoardUnavailable, "router board error: {detail}"));
        }
        bail!(coded!(
            BoardApiMismatch,
            "router omitted its database path; restart trufflepig-system.service"
        ));
    };
    let database = PathBuf::from(path);
    ensure!(
        database.is_absolute(),
        coded!(BoardApiMismatch, "router database path is not absolute")
    );
    Ok((api, database))
}

fn local_reply(
    options: &BoardArguments,
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
    if runtime.is_some_and(|runtime| tp_core::daemon_transport::running(runtime)) {
        if matches!(command, BoardCommand::Op(crate::BoardOp::Feedback { .. })) {
            check_deadline(deadline)?;
            let config = load()?;
            check_deadline(deadline)?;
            return crate::unavailable_or_queue(
                command,
                options,
                context,
                &config,
                spool,
                deadline,
                anyhow::anyhow!(coded!(BoardUnavailable, "router owns migration")),
            );
        }
        check_deadline(deadline)?;
        let config = load()?;
        check_deadline(deadline)?;
        if let Some(runtime) = runtime {
            crate::board_runtime_markers::validate_board_database(runtime, &config.db_path)?;
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
        crate::board_runtime_markers::validate_board_database(runtime, &config.db_path)?;
    }
    run_local(command, options, context, &config, runtime, spool, deadline)
}

/// Opens read-only beside a live router, so missing or stale storage refuses
/// without creating or migrating it; current files proceed to the local run.
fn refuse_stale_storage_beside_router(
    command: &BoardCommand,
    options: &BoardArguments,
    context: &RequestContext,
    config: &BoardConfig,
    runtime: Option<&Path>,
    spool: &Path,
    deadline: &QueryDeadline,
) -> Result<()> {
    use crate::local_board::LocalBoard;
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
            bail!(coded!(BoardUnavailable, "router owns migration"))
        }
        Err(error) => {
            let error = anyhow::Error::from(error);
            if local_access::is_storage_access_failure(&error) {
                return Err(local_access::storage_access_advice(error, config, runtime));
            }
            crate::unavailable_or_queue(command, options, context, config, spool, deadline, error)
                .map(|_| ())
        }
    }
}

fn run_local(
    command: &BoardCommand,
    options: &BoardArguments,
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
                && !matches!(command, BoardCommand::Op(crate::BoardOp::Feedback { .. })) =>
        {
            Err(local_access::storage_access_advice(error, config, runtime))
        }
        Err(error) => {
            crate::unavailable_or_queue(command, options, context, config, spool, deadline, error)
        }
    }
}

#[cfg(test)]
mod tests;
