//! Parses Board v10 grant, worker, identity, and evidence commands.
use super::{
    BoardTextPayload,
    board_syntax_validation::{check_flags, fixed_words, optional_text, recipient, word},
};
use crate::{
    board::board_grammar::BoardArguments,
    board::{
        board_ids::{BoardRef, EntryId},
        board_protocol::{
            BoardOp, GateStatus, ProgressGateInput, ProgressInput, ProgressStep, ProgressTests,
            ReviewVerdict, WorkerAddress, WorkerHandle,
        },
        board_vocabulary::{AuthorityRole, EntryKind, EntryText, WorkerEndReason, WorkerRole},
    },
};
use anyhow::{Result, bail, ensure};
use tp_core::coded_error::CodedContext;
use tp_core::identity::GitOid;

pub(super) fn parse_v10(
    options: &BoardArguments,
    payload: &BoardTextPayload,
    verb: &str,
) -> Result<Option<BoardOp>> {
    let op = match verb {
        "hello" => parse_hello(options)?,
        "post" => parse_post(options, payload)?,
        "grant" => parse_grant(options, payload)?,
        "revoke" => parse_revoke(options)?,
        "bye" => {
            check_flags(options, &[])?;
            fixed_words(options, 2, 2, "board bye")?;
            BoardOp::Bye
        }
        "worker" => parse_worker(options, payload)?,
        _ => return Ok(None),
    };
    Ok(Some(op))
}

fn parse_hello(options: &BoardArguments) -> Result<BoardOp> {
    check_flags(options, &["worker"])?;
    fixed_words(
        options,
        3,
        4,
        "board hello MODEL [EFFORT] [--worker P7.3/HANDLE]",
    )?;
    Ok(BoardOp::Hello {
        model: word(options, 2, "board hello MODEL [EFFORT]")?.to_owned(),
        effort: options.words.get(3).cloned(),
        worker: options
            .board
            .worker
            .as_deref()
            .map(WorkerAddress::parse)
            .transpose()?,
    })
}

fn parse_post(options: &BoardArguments, payload: &BoardTextPayload) -> Result<BoardOp> {
    check_flags(
        options,
        &[
            "to",
            "supersedes",
            "text",
            "head",
            "step",
            "tests",
            "gate",
            "eta",
            "verdict",
        ],
    )?;
    let target = word(options, 2, "board post P7[.3] KIND TEXT")?.parse::<BoardRef>()?;
    let kind = word(options, 3, "board post P7[.3] KIND TEXT")?.parse::<EntryKind>()?;
    let progress = progress_input(options)?;
    let verdict = options
        .board
        .verdict
        .as_deref()
        .map(ReviewVerdict::parse)
        .transpose()?;
    Ok(BoardOp::Post {
        target,
        kind,
        body: EntryText::new(payload.text.clone())?,
        to: recipient(options)?,
        supersedes: options
            .board
            .supersedes
            .as_deref()
            .map(str::parse::<EntryId>)
            .transpose()?,
        progress,
        verdict,
    })
}

fn progress_input(options: &BoardArguments) -> Result<Option<ProgressInput>> {
    let board = &options.board;
    let has_input = board.head.is_some()
        || board.step.is_some()
        || board.tests.is_some()
        || !board.gates.is_empty()
        || board.eta.is_some();
    if !has_input {
        return Ok(None);
    }
    let step = board
        .step
        .as_deref()
        .map(|value| {
            let (current, total) = value
                .split_once('/')
                .with_coded_context(|| coded!(InvalidProgress, "--step must be K/N"))?;
            ensure!(
                !total.contains('/'),
                coded!(InvalidProgress, "--step must be K/N")
            );
            Ok::<_, anyhow::Error>(ProgressStep {
                current: current.parse()?,
                total: total.parse()?,
            })
        })
        .transpose()?;
    let tests = board
        .tests
        .as_deref()
        .map(|value| {
            let (passed, failed) = value
                .split_once('/')
                .with_coded_context(|| coded!(InvalidProgress, "--tests must be PASSED/FAILED"))?;
            ensure!(
                !failed.contains('/'),
                coded!(InvalidProgress, "--tests must be PASSED/FAILED")
            );
            Ok::<_, anyhow::Error>(ProgressTests {
                passed: passed.parse()?,
                failed: failed.parse()?,
            })
        })
        .transpose()?;
    let gates = board
        .gates
        .iter()
        .map(|value| {
            let (name, status) = value.split_once('=').with_coded_context(|| {
                coded!(InvalidProgress, "--gate must be NAME=pass|fail|running")
            })?;
            ensure!(
                !status.contains('='),
                coded!(InvalidProgress, "--gate must be NAME=pass|fail|running")
            );
            let status = match status {
                "pass" => GateStatus::Pass,
                "fail" => GateStatus::Fail,
                "running" => GateStatus::Running,
                _ => bail!(coded!(InvalidProgress, "unknown gate status '{status}'")),
            };
            Ok::<_, anyhow::Error>(ProgressGateInput {
                name: name.to_owned(),
                status,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let input = ProgressInput {
        head: board.head.as_deref().map(GitOid::parse).transpose()?,
        step,
        tests,
        gates,
        eta_secs: board.eta.as_deref().map(parse_duration).transpose()?,
    };
    input.validate()?;
    Ok(Some(input))
}

fn parse_grant(options: &BoardArguments, payload: &BoardTextPayload) -> Result<BoardOp> {
    check_flags(options, &["board", "to", "for", "text"])?;
    let grantee = recipient(options)?
        .with_coded_context(|| coded!(InvalidOptions, "grant requires --to WHO"))?;
    let expires_in = options
        .board
        .for_value
        .as_deref()
        .map(parse_duration)
        .transpose()?;
    if let Some(scope) = &options.board.board_scope {
        fixed_words(options, 2, 2, "board grant --board triage --to WHO")?;
        ensure!(
            scope == "triage",
            coded!(InvalidOptions, "--board supports only triage")
        );
        return Ok(BoardOp::Grant {
            plan: None,
            roles: vec![AuthorityRole::Triage],
            grantee,
            expires_in,
            note: optional_text(&payload.text)?,
        });
    }
    fixed_words(
        options,
        4,
        usize::MAX,
        "board grant P7 ROLE[,ROLE...] --to WHO [--for DURATION] [NOTE...]",
    )?;
    let plan =
        match word(options, 2, "board grant P7 ROLE[,ROLE...] --to WHO")?.parse::<BoardRef>()? {
            BoardRef::Plan(plan) => plan,
            _ => bail!(coded!(InvalidReference, "grant requires a plan P#")),
        };
    let roles = word(options, 3, "board grant P7 ROLE[,ROLE...] --to WHO")?
        .split(',')
        .map(AuthorityRole::parse)
        .collect::<Result<Vec<_>>>()?;
    Ok(BoardOp::Grant {
        plan: Some(plan),
        roles,
        grantee,
        expires_in,
        note: optional_text(&payload.text)?,
    })
}

fn parse_revoke(options: &BoardArguments) -> Result<BoardOp> {
    check_flags(options, &[])?;
    fixed_words(options, 3, 3, "board revoke E#")?;
    Ok(BoardOp::Revoke {
        entry: word(options, 2, "board revoke E#")?.parse::<EntryId>()?,
    })
}

fn parse_worker(options: &BoardArguments, payload: &BoardTextPayload) -> Result<BoardOp> {
    let task = match word(options, 2, "board worker P7.3 add|end ...")?.parse::<BoardRef>()? {
        BoardRef::Task(task) => task,
        _ => bail!(coded!(InvalidReference, "worker requires a task P#.N")),
    };
    match options.words.get(3).map(String::as_str) {
        Some("add") => {
            check_flags(options, &["role", "model", "effort", "scope", "files"])?;
            fixed_words(
                options,
                5,
                5,
                "board worker P7.3 add HANDLE --role ROLE --model MODEL --scope TEXT --files PATH",
            )?;
            let handle = WorkerHandle::parse(word(options, 4, "board worker P7.3 add HANDLE")?)?;
            let role = options
                .board
                .worker_role
                .as_deref()
                .map(WorkerRole::parse)
                .transpose()?
                .with_coded_context(|| coded!(InvalidOptions, "worker add requires --role"))?;
            let model = options
                .board
                .model
                .clone()
                .with_coded_context(|| coded!(InvalidOptions, "worker add requires --model"))?;
            let effort = options.board.effort.clone();
            let scope =
                EntryText::new(options.board.scope.clone().with_coded_context(|| {
                    coded!(InvalidOptions, "worker add requires --scope")
                })?)?;
            let files = options
                .board
                .files
                .iter()
                .flat_map(|list| list.split(','))
                .map(str::trim)
                .map(str::to_owned)
                .collect();
            Ok(BoardOp::WorkerAdd {
                task,
                handle,
                role,
                model,
                effort,
                scope,
                files,
            })
        }
        Some("end") => {
            check_flags(options, &["text"])?;
            fixed_words(
                options,
                6,
                usize::MAX,
                "board worker P7.3 end HANDLE done|failed|stopped [NOTE...]",
            )?;
            let handle =
                WorkerHandle::parse(word(options, 4, "board worker P7.3 end HANDLE STATE")?)?;
            let state = match word(options, 5, "board worker P7.3 end HANDLE STATE")? {
                "done" => WorkerEndReason::Done,
                "failed" => WorkerEndReason::Failed,
                "stopped" => WorkerEndReason::Stopped,
                other => bail!(coded!(
                    InvalidState,
                    "worker end expects done, failed, or stopped, received '{other}'"
                )),
            };
            Ok(BoardOp::WorkerEnd {
                worker: WorkerAddress::new(task, handle)?,
                state,
                note: optional_text(&payload.text)?,
            })
        }
        _ => bail!(coded!(
            Usage,
            "board worker P7.3 add HANDLE ... | end HANDLE STATE [NOTE...]"
        )),
    }
}

fn parse_duration(value: &str) -> Result<u64> {
    let (digits, multiplier) = match value.chars().last() {
        Some('s') => (&value[..value.len() - 1], 1),
        Some('m') => (&value[..value.len() - 1], 60),
        Some('h') => (&value[..value.len() - 1], 3_600),
        Some('d') => (&value[..value.len() - 1], 86_400),
        _ => (value, 1),
    };
    ensure!(
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
        coded!(InvalidDuration, "use positive seconds or Ns, Nm, Nh, or Nd")
    );
    let seconds = digits
        .parse::<u64>()?
        .checked_mul(multiplier)
        .with_coded_context(|| coded!(InvalidDuration, "duration is too large"))?;
    ensure!(
        (1..=crate::board::board_protocol::PROGRESS_ETA_LIMIT_SECS).contains(&seconds),
        coded!(InvalidDuration, "duration must be 1..=365 days")
    );
    Ok(seconds)
}
