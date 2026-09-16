//! One client emission boundary for execution, parsing, and delivery failures.
use super::emitted_evidence::{SavedEntries, capture_emitted};
use super::{Arguments, request_context};
use crate::diagnostics::{DiagnosticsMode, Operation, Outcome, RequestEvent};
use crate::output::OutputBudget;
use clap::Parser;
use serde_json::{Value, json};
use std::{io::Write, time::Instant};

pub fn execute(args: &[String], stdout: &mut impl Write, stderr: &mut impl Write) -> i32 {
    let started = Instant::now();
    let mut stderr = CountingWriter {
        writer: stderr,
        accepted: 0,
    };
    let mut parsed =
        Arguments::try_parse_from(std::iter::once("trufflepig".into()).chain(args.iter().cloned()));
    if let Ok(options) = &mut parsed {
        options.explicit_root = !options.implicit_root
            && args
                .iter()
                .any(|arg| arg == "--root" || arg.starts_with("--root="));
    }
    let fallback = fallback_options(args);
    let options = parsed.as_ref().unwrap_or(&fallback);
    let context = request_context(options);
    let diagnostic_location = crate::workspace::diagnostic_location(options);
    let mut operation = operation(options);
    let (response, outcome, mut exit_code) = match &parsed {
        Ok(_) => match super::run_with_context(args, &context) {
            Ok(text) => (text, Outcome::Success, 0),
            Err(error) => {
                let message = format!("{error:#}");
                let outcome = error_outcome(&message);
                let stderr_message = message.replace(['\n', '\r'], " ");
                let stderr_message = if stderr_message.contains("budget_too_small") {
                    format!("{stderr_message}; {}", retry_hint(options.budget))
                } else {
                    stderr_message
                };
                let _ = writeln!(stderr, "{stderr_message}");
                (
                    render(options.budget, &json!({"error":message})),
                    outcome,
                    2,
                )
            }
        },
        Err(error) => {
            let (key, code) = match error.kind() {
                clap::error::ErrorKind::DisplayHelp => {
                    operation = Operation::Help;
                    ("help", 0)
                }
                clap::error::ErrorKind::DisplayVersion => {
                    operation = Operation::Version;
                    ("version", 0)
                }
                _ => {
                    operation = Operation::Usage;
                    ("error", 2)
                }
            };
            let message = error.to_string();
            if code != 0 {
                let _ = write!(stderr, "{message}");
            }
            (
                if key == "help" {
                    OutputBudget::new(options.budget.min(1_000_000)).and_then(|budget| budget.render(&json!({key:message}))).unwrap_or_else(|_| render(options.budget, &json!({"help":"Commands: ws show, ws status, ws discover PATH..., search, show, more, ctx, refs, map, index, status, doctor, hist-index, hist-status, hist, since, diff, blame, session start, session end ID, audit, forget-logs, stop, system status, system ensure, system stop, system prune. Options: --workspace, --no-workspace, --member, --root, --cache, --history-cache, --no-daemon, --budget (default 600), --session, --diagnostics off|metadata|detailed.","truncated":true,"details":"Use --help -b 2000 for full option descriptions"})))
                } else {
                    render(options.budget, &json!({key:message}))
                },
                if code == 0 {
                    Outcome::Success
                } else {
                    Outcome::InvalidInput
                },
                code,
            )
        }
    };
    let response = if options.budget == 0 {
        String::new()
    } else {
        response
    };
    let mut event = RequestEvent::new(context, operation, outcome);
    event.stderr_accepted_bytes = stderr.accepted;
    event.elapsed_micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    match crate::diagnostics::emit_response(stdout, &response) {
        Ok(receipt) => {
            if !receipt.complete {
                exit_code = 2;
            }
            event.receipt = Some(receipt);
        }
        Err(_) => exit_code = 2,
    }
    // Logging cannot replace or retry the response, including partial delivery.
    if !matches!(operation, Operation::ForgetLogs)
        && let Ok((root, cache, workspace, member)) = diagnostic_location
    {
        event.workspace = workspace.clone();
        event.member = member;
        let saved = if workspace.is_some() {
            crate::workspace::resolve(options)
                .ok()
                .flatten()
                .and_then(|config| {
                    let cache_base = options
                        .cache
                        .as_deref()
                        .and_then(|path| std::path::absolute(path).ok());
                    crate::workspace::cache_path(&config, cache_base.as_deref()).ok()
                })
                .map(|workspace_cache| SavedEntries::workspace(&root, &workspace_cache))
        } else {
            Some(SavedEntries::singleton(&root, &cache))
        };
        capture_emitted(
            &root,
            &response,
            &mut event,
            saved.as_ref(),
            options.words.get(1).map(String::as_str),
        );
        if matches!(operation, Operation::Search) && options.diagnostics == "detailed" {
            event.raw_query = Some(options.words[1..].join(" "));
        }
        // Deletion itself must leave no fresh journal or revived session behind.
        if !matches!(operation, Operation::ForgetLogs) {
            let _ =
                crate::diagnostics::best_effort_record(&cache, diagnostics_mode(options), event);
        }
    }
    exit_code
}

pub(super) fn diagnostics_mode(options: &Arguments) -> DiagnosticsMode {
    match options.diagnostics.as_str() {
        "off" => DiagnosticsMode::Off,
        "detailed" => DiagnosticsMode::Detailed,
        _ => DiagnosticsMode::Metadata,
    }
}

pub(super) fn operation(options: &Arguments) -> Operation {
    match options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status")
    {
        "show" => Operation::Show,
        "ctx" => Operation::Context,
        "more" => Operation::More,
        "refs" => Operation::References,
        "map" => Operation::Map,
        verb if verb.starts_with("refs:") => Operation::References,
        "hist-index" => Operation::HistoryIndex,
        "hist-status" => Operation::HistoryStatus,
        "hist" => Operation::History,
        "since" => Operation::Since,
        "diff" => Operation::Diff,
        "blame" => Operation::Blame,
        "index" | "init" => Operation::Index,
        "status" => Operation::Status,
        "doctor" => Operation::Doctor,
        "session" if options.words.get(1).is_some_and(|s| s == "start") => Operation::SessionStart,
        "session" => Operation::SessionEnd,
        "audit" => Operation::Audit,
        "forget-logs" => Operation::ForgetLogs,
        "search" => Operation::Search,
        "serve" | "history-serve" | "stop" | "ws" | "workspace-serve" | "system"
        | "system-serve" | "semantic-check" => Operation::Other,
        _ => Operation::Usage,
    }
}

fn error_outcome(message: &str) -> Outcome {
    if message.contains("budget_too_small") {
        Outcome::BudgetTooSmall
    } else if message.contains("stale_") {
        Outcome::Stale
    } else if message.contains("unavailable") || message.contains("expired_") {
        Outcome::Unavailable
    } else if message.contains("invalid_")
        || message.contains("unknown_command")
        || message.contains("usage:")
    {
        Outcome::InvalidInput
    } else {
        Outcome::Failure
    }
}

fn retry_budget(current: usize) -> usize {
    current.saturating_mul(2).max(2_000).min(1_000_000)
}

fn retry_hint(current: usize) -> String {
    let suggestion = retry_budget(current);
    if suggestion > current {
        format!("retry with -b {suggestion}")
    } else {
        "maximum budget is 1000000; narrow the request".into()
    }
}

fn render(limit: usize, value: &Value) -> String {
    let Ok(budget) = OutputBudget::new(limit.min(1_000_000)) else {
        return String::new();
    };
    budget
        .render(value)
        .or_else(|_| budget.render(&json!({"error":"budget_too_small"})))
        .unwrap_or_default()
}

fn fallback_options(args: &[String]) -> Arguments {
    let mut options = Arguments::try_parse_from(["trufflepig"]).expect("default arguments");
    for (index, arg) in args.iter().enumerate() {
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(a, b)| (a, Some(b)));
        let value = inline.or_else(|| args.get(index + 1).map(String::as_str));
        if let Some(value) = value {
            match flag {
                "--root" => options.root = value.into(),
                "--cache" => options.cache = Some(value.into()),
                "--diagnostics" => options.diagnostics = value.into(),
                "--session" => options.session = Some(value.into()),
                "--client" => options.client = Some(value.into()),
                "--budget" | "-b" => {
                    if let Ok(limit) = value.parse() {
                        options.budget = limit;
                    }
                }
                _ => (),
            }
        }
    }
    options
}

struct CountingWriter<'a, W> {
    writer: &'a mut W,
    accepted: usize,
}
impl<W: Write> Write for CountingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.writer.write(bytes)?;
        self.accepted = self.accepted.saturating_add(count);
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}
