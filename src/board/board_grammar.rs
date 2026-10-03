//! Typed board commands and their client-owned text transport.

use super::{
    board_actor::{BoardRecipient, HarnessLabel},
    board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision},
    board_protocol::{BoardOp, FeedbackMetadata, RecentCall},
    board_vocabulary::{
        EntryKind, EntryText, FeedbackImportKey, FeedbackKind, FeedbackState, PlanText, PlanTitle, TaskColumn,
    },
};
use crate::cli::Arguments;
use anyhow::{Context, Result, bail};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Options shared by the board and feedback command families.
#[derive(Args, Debug, Clone, Default)]
pub struct BoardOptions {
    /// Read a plan or feedback body from a file, or `-` for stdin.
    #[arg(long)]
    pub body: Option<PathBuf>,
    /// Address a user or harness.
    #[arg(long)]
    pub to: Option<String>,
    /// Entry corrected by this post.
    #[arg(long)]
    pub supersedes: Option<String>,
    /// Harness entrusted with accepting plan proposals.
    #[arg(long)]
    pub steward: Option<String>,
    /// Attach feedback to a plan.
    #[arg(long)]
    pub plan: Option<String>,
    /// Scope of work covered by a new claimed task.
    #[arg(long)]
    pub scope: Option<String>,
    /// Plan heading covered by a new claimed task.
    #[arg(long)]
    pub section: Option<String>,
    /// List only open feedback reports.
    #[arg(long)]
    pub open: bool,
    /// Client-normalized free text and optional file body.
    #[arg(long, hide = true, require_equals = true, allow_hyphen_values = true)]
    pub board_text: Option<String>,
    /// Model claim captured by the agent wrapper.
    #[arg(long, hide = true)]
    pub agent_model: Option<String>,
    /// Effort claim captured by the agent wrapper.
    #[arg(long, hide = true)]
    pub agent_effort: Option<String>,
    /// Bounded recent call audit captured by the agent wrapper.
    #[arg(long, hide = true)]
    pub recent_calls: Option<String>,
}

impl BoardOptions {
    /// Reject board-specific options outside their command families.
    pub fn validate_for_verb(&self, verb: Option<&str>) -> Result<()> {
        if !matches!(verb, Some("board" | "feedback")) && self.has_options() {
            bail!("invalid_options: board options require board or feedback");
        }
        if self
            .recent_calls
            .as_ref()
            .is_some_and(|text| text.len() > 2_048)
        {
            bail!("invalid_options: --recent-calls exceeds 2048 bytes");
        }
        Ok(())
    }

    /// Preserve board values when a request is routed through the daemon.
    pub fn forward(&self, args: &mut Vec<String>) {
        for (flag, value) in [
            (
                "--body",
                self.body
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
            ("--to", self.to.clone()),
            ("--supersedes", self.supersedes.clone()),
            ("--steward", self.steward.clone()),
            ("--plan", self.plan.clone()),
            ("--scope", self.scope.clone()),
            ("--section", self.section.clone()),
            ("--board-text", self.board_text.clone()),
            ("--agent-model", self.agent_model.clone()),
            ("--agent-effort", self.agent_effort.clone()),
            ("--recent-calls", self.recent_calls.clone()),
        ] {
            if let Some(value) = value {
                args.push(format!("{flag}={value}"));
            }
        }
        if self.open {
            args.push("--open".into());
        }
    }

    fn has_options(&self) -> bool {
        self.body.is_some()
            || self.to.is_some()
            || self.supersedes.is_some()
            || self.steward.is_some()
            || self.plan.is_some()
            || self.scope.is_some()
            || self.section.is_some()
            || self.open
            || self.board_text.is_some()
            || self.agent_model.is_some()
            || self.agent_effort.is_some()
            || self.recent_calls.is_some()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BoardTextPayload {
    text: String,
    body: Option<String>,
    import_key: Option<FeedbackImportKey>,
    steer_mode: Option<String>,
}

/// Backend operations and the one local git-ingestion command.
#[derive(Clone, Debug, PartialEq)]
pub enum BoardCommand {
    Op(BoardOp),
    Ingest,
}

/// Remove client paths and free positional text before router parsing.
pub fn normalize_args(
    _args: &[String],
    options: &Arguments,
    body: Option<&str>,
) -> Result<Vec<String>> {
    let command = parse(options, body)?;
    let mut payload = command_text(options, body)?;
    if let BoardCommand::Op(BoardOp::Feedback { import_key, .. }) = command {
        payload.import_key = import_key;
    }
    if options.words.first().map(String::as_str) == Some("feedback")
        && options
            .board
            .board_text
            .as_deref()
            .and_then(|value| serde_json::from_str::<BoardTextPayload>(value).ok())
            .is_none()
    {
        payload.steer_mode = std::env::var("TRUFFLEPIG_AGENT_STEER").ok();
    }
    let mut forwarded = options.clone();
    if let Some(start) = text_position(options) {
        forwarded.words.truncate(start);
    }
    forwarded.board.body = None;
    forwarded.board.board_text = if text_position(options).is_some() || payload.body.is_some() {
        Some(serde_json::to_string(&payload)?)
    } else {
        None
    };
    let root = options
        .root
        .canonicalize()
        .context("invalid_root: cannot open board cwd")?;
    Ok(crate::cli::normalized_args(&forwarded, &root))
}

fn text_position(options: &Arguments) -> Option<usize> {
    let words = &options.words;
    match (
        words.first().map(String::as_str),
        words.get(1).map(String::as_str),
    ) {
        (Some("board"), Some("new")) => Some(2),
        (Some("board"), Some("claim" | "propose" | "edit" | "accept" | "reject")) => Some(3),
        (Some("board"), Some("post")) => Some(4),
        (Some("board"), Some("task"))
            if words.get(2).is_some_and(|target| !target.contains('.')) =>
        {
            Some(3)
        }
        (Some("feedback"), Some("blocked" | "confused" | "wrong" | "missing")) => Some(2),
        (Some("feedback"), Some("close")) => Some(4),
        _ => None,
    }
}

fn command_text(options: &Arguments, body: Option<&str>) -> Result<BoardTextPayload> {
    let positional = text_position(options)
        .map(|start| options.words.get(start..).unwrap_or_default().join(" "))
        .unwrap_or_default();
    if let Some(hidden) = &options.board.board_text {
        if !positional.is_empty() {
            bail!("invalid_options: positional text conflicts with --board-text");
        }
        if let Ok(mut payload) = serde_json::from_str::<BoardTextPayload>(hidden) {
            if body.is_some() && payload.body.is_some() {
                bail!("invalid_options: body supplied twice");
            }
            payload.body = payload.body.or_else(|| body.map(str::to_owned));
            return Ok(payload);
        }
        return Ok(BoardTextPayload {
            text: hidden.clone(),
            body: body.map(str::to_owned),
            import_key: None,
            steer_mode: None,
        });
    }
    Ok(BoardTextPayload {
        text: positional,
        body: body.map(str::to_owned),
        import_key: None,
        steer_mode: None,
    })
}

/// Parse board/feedback words without reading client-owned files.
pub fn parse(options: &Arguments, body: Option<&str>) -> Result<BoardCommand> {
    let verb = options.words.first().map(String::as_str);
    options.board.validate_for_verb(verb)?;
    let payload = command_text(options, body)?;
    if options.board.body.is_some() && payload.body.is_none() {
        bail!("invalid_body: --body must be read by the client before parsing");
    }
    if payload.body.is_some()
        && !matches!(
            (verb, options.words.get(1).map(String::as_str)),
            (Some("board"), Some("new" | "propose" | "edit"))
                | (
                    Some("feedback"),
                    Some("blocked" | "confused" | "wrong" | "missing")
                )
        )
    {
        bail!("invalid_options: a body is not valid for this board command");
    }
    if payload.import_key.is_some()
        && !matches!(
            (verb, options.words.get(1).map(String::as_str)),
            (
                Some("feedback"),
                Some("blocked" | "confused" | "wrong" | "missing")
            )
        )
    {
        bail!("invalid_options: an import key is only valid for feedback reports");
    }
    let command = match verb {
        Some("board") => parse_board(options, &payload)?,
        Some("feedback") => parse_feedback(options, payload)?,
        _ => bail!("usage: board COMMAND | feedback KIND SUMMARY"),
    };
    if let BoardCommand::Op(op) = &command {
        op.validate()?;
    }
    Ok(command)
}

fn parse_board(options: &Arguments, payload: &BoardTextPayload) -> Result<BoardCommand> {
    let words = &options.words;
    let verb = words.get(1).map(String::as_str).unwrap_or("inbox");
    let op = match verb {
        "hello" => {
            check_flags(options, &[])?;
            fixed_words(options, 3, 4, "board hello MODEL [EFFORT]")?;
            BoardOp::Hello {
                model: word(options, 2, "board hello MODEL [EFFORT]")?.into(),
                effort: words.get(3).cloned(),
            }
        }
        "inbox" => return parse_inbox(options, 2),
        "show" => {
            check_flags(options, &[])?;
            fixed_words(options, 2, 3, "board show [P7|P7@12|P7@10..14]")?;
            let target = words
                .get(2)
                .map(|value| value.parse::<BoardRef>())
                .transpose()?;
            if matches!(
                target,
                Some(BoardRef::Task(_) | BoardRef::Entry(_) | BoardRef::Commit(_))
            ) {
                bail!("invalid_reference: show requires a plan, revision, or revision span");
            }
            BoardOp::Show { target }
        }
        "new" => {
            check_flags(options, &["body", "steward", "text"])?;
            BoardOp::New {
                title: PlanTitle::new(payload.text.clone())?,
                body: PlanText::new(payload.body.clone().unwrap_or_default())?,
                steward: options
                    .board
                    .steward
                    .as_deref()
                    .map(HarnessLabel::parse)
                    .transpose()?,
            }
        }
        "post" => {
            check_flags(options, &["to", "supersedes", "text"])?;
            let target = word(options, 2, "board post P7[.3] KIND TEXT")?.parse::<BoardRef>()?;
            let kind = word(options, 3, "board post P7[.3] KIND TEXT")?.parse::<EntryKind>()?;
            BoardOp::Post {
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
            }
        }
        "task" => parse_task(options, payload)?,
        "claim" => parse_claim(options, payload)?,
        "propose" | "edit" => {
            check_flags(options, &["body", "text"])?;
            let base = word(options, 2, "board propose|edit P7@12 --body FILE SUMMARY")?
                .parse::<PlanRevision>()?;
            let body = PlanText::new(
                payload
                    .body
                    .clone()
                    .context("invalid_body: propose and edit require --body FILE")?,
            )?;
            let summary = EntryText::new(payload.text.clone())?;
            if verb == "propose" {
                BoardOp::Propose {
                    base,
                    body,
                    summary,
                }
            } else {
                BoardOp::Edit {
                    base,
                    body,
                    summary,
                }
            }
        }
        "accept" | "reject" => {
            check_flags(options, &["text"])?;
            let proposal =
                word(options, 2, "board accept|reject E485 [NOTE]")?.parse::<EntryId>()?;
            if verb == "accept" {
                BoardOp::Accept {
                    proposal,
                    note: optional_text(&payload.text)?,
                }
            } else {
                BoardOp::Reject {
                    proposal,
                    reason: EntryText::new(payload.text.clone())?,
                }
            }
        }
        "review" => {
            check_flags(options, &[])?;
            fixed_words(options, 3, 4, "board review P7@12 [AGENT]")?;
            BoardOp::Review {
                base: word(options, 2, "board review P7@12 [AGENT]")?.parse::<PlanRevision>()?,
                agent: words
                    .get(3)
                    .map(|value| HarnessLabel::parse(value))
                    .transpose()?,
            }
        }
        "ingest" => {
            check_flags(options, &[])?;
            fixed_words(options, 2, 2, "board ingest")?;
            return Ok(BoardCommand::Ingest);
        }
        value if value.parse::<u64>().is_ok() => return parse_inbox(options, 1),
        _ => bail!(
            "usage: board hello|inbox|show|claim|post|task|propose|review|accept|reject|edit|new|ingest"
        ),
    };
    Ok(BoardCommand::Op(op))
}

fn parse_inbox(options: &Arguments, index: usize) -> Result<BoardCommand> {
    check_flags(options, &["wait"])?;
    let minimum = if options.words.len() == 1 { 1 } else { index };
    fixed_words(options, minimum, index + 1, "board [inbox] [SEQ] [--wait]")?;
    let after = options
        .words
        .get(index)
        .map(|value| value.parse::<EventSeq>())
        .transpose()?;
    Ok(BoardCommand::Op(BoardOp::Inbox {
        after,
        limit: options.limit,
    }))
}

fn parse_task(options: &Arguments, payload: &BoardTextPayload) -> Result<BoardOp> {
    let target = word(options, 2, "board task P7 TITLE | P7.3 COLUMN")?.parse::<BoardRef>()?;
    match target {
        BoardRef::Plan(plan) => {
            check_flags(options, &["to", "text"])?;
            Ok(BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new(payload.text.clone())?,
                to: recipient(options)?,
                section: None,
            })
        }
        BoardRef::Task(task) => {
            check_flags(options, &["to"])?;
            fixed_words(
                options,
                4,
                4,
                "board task P7.3 todo|doing|review|done|blocked",
            )?;
            Ok(BoardOp::TaskMove {
                task,
                column: word(options, 3, "board task P7.3 COLUMN")?.parse::<TaskColumn>()?,
                to: recipient(options)?,
            })
        }
        _ => bail!("invalid_reference: task requires a plan or task"),
    }
}

fn parse_claim(options: &Arguments, payload: &BoardTextPayload) -> Result<BoardOp> {
    let target = word(
        options,
        2,
        "board claim P7.3 SCOPE | P7 TITLE --scope SCOPE",
    )?
    .parse::<BoardRef>()?;
    match target {
        BoardRef::Plan(plan) => {
            check_flags(options, &["scope", "section", "text"])?;
            Ok(BoardOp::CarveClaim {
                plan,
                title: PlanTitle::new(payload.text.clone())?,
                scope: EntryText::new(
                    options
                        .board
                        .scope
                        .clone()
                        .context("invalid_options: claiming a plan requires --scope")?,
                )?,
                section: options.board.section.clone(),
            })
        }
        BoardRef::Task(task) => {
            check_flags(options, &["text"])?;
            Ok(BoardOp::ClaimTask {
                task,
                scope: EntryText::new(payload.text.clone())?,
            })
        }
        _ => bail!("invalid_reference: claim requires a plan or task"),
    }
}

fn parse_feedback(options: &Arguments, payload: BoardTextPayload) -> Result<BoardCommand> {
    let verb = word(
        options,
        1,
        "feedback KIND SUMMARY | ls | close E512 STATE [NOTE]",
    )?;
    let op = match verb {
        "ls" => {
            check_flags(options, &["open"])?;
            fixed_words(options, 2, 2, "feedback ls [--open]")?;
            BoardOp::FeedbackList {
                open_only: options.board.open,
            }
        }
        "close" => {
            check_flags(options, &["text"])?;
            BoardOp::FeedbackClose {
                entry: word(options, 2, "feedback close E512 STATE [NOTE]")?.parse::<EntryId>()?,
                state: word(options, 3, "feedback close E512 STATE [NOTE]")?
                    .parse::<FeedbackState>()?,
                note: optional_text(&payload.text)?,
            }
        }
        _ => {
            check_flags(options, &["body", "plan", "recent-calls", "text"])?;
            let import_key = payload.import_key.unwrap_or_else(FeedbackImportKey::new);
            BoardOp::Feedback {
                kind: verb.parse::<FeedbackKind>()?,
                summary: EntryText::new(payload.text)?,
                body: payload.body.map(EntryText::new).transpose()?,
                plan: options
                    .board
                    .plan
                    .as_deref()
                    .map(str::parse::<PlanId>)
                    .transpose()?,
                metadata: feedback_metadata(options, payload.steer_mode)?,
                import_key: Some(import_key),
            }
        }
    };
    Ok(BoardCommand::Op(op))
}

fn feedback_metadata(options: &Arguments, steer_mode: Option<String>) -> Result<FeedbackMetadata> {
    let recent_calls = options
        .board
        .recent_calls
        .as_deref()
        .map(|value| {
            serde_json::from_str::<Vec<RecentCall>>(value)
                .context("invalid_options: --recent-calls must be a JSON call list")
        })
        .transpose()?
        .unwrap_or_default();
    Ok(FeedbackMetadata {
        version: env!("CARGO_PKG_VERSION").into(),
        build_id: option_env!("TRUFFLEPIG_BUILD_ID").map(str::to_owned),
        repo_key: None,
        cwd: options.root.to_string_lossy().into_owned(),
        steer_mode,
        recent_calls,
    })
}

fn check_flags(options: &Arguments, allowed: &[&str]) -> Result<()> {
    let board = &options.board;
    for (name, present) in [
        ("body", board.body.is_some()),
        ("to", board.to.is_some()),
        ("supersedes", board.supersedes.is_some()),
        ("steward", board.steward.is_some()),
        ("plan", board.plan.is_some()),
        ("scope", board.scope.is_some()),
        ("section", board.section.is_some()),
        ("open", board.open),
        ("text", board.board_text.is_some()),
        ("recent-calls", board.recent_calls.is_some()),
        ("wait", options.wait),
    ] {
        if present && !allowed.contains(&name) {
            let flag = if name == "text" { "board-text" } else { name };
            bail!("invalid_options: --{flag} is not valid for this board command");
        }
    }
    Ok(())
}

fn recipient(options: &Arguments) -> Result<Option<BoardRecipient>> {
    options
        .board
        .to
        .as_deref()
        .map(BoardRecipient::parse)
        .transpose()
}

fn optional_text(value: &str) -> Result<Option<EntryText>> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        EntryText::new(value.to_owned()).map(Some)
    }
}

fn word<'a>(options: &'a Arguments, index: usize, usage: &str) -> Result<&'a str> {
    options
        .words
        .get(index)
        .map(String::as_str)
        .with_context(|| format!("usage: {usage}"))
}

fn fixed_words(options: &Arguments, minimum: usize, maximum: usize, usage: &str) -> Result<()> {
    if !(minimum..=maximum).contains(&options.words.len()) {
        bail!("usage: {usage}");
    }
    Ok(())
}
