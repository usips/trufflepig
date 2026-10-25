//! Parses feedback reports, triage, closure, and wrapper audit metadata.
use super::{
    BoardCommand, BoardTextPayload, board_read_parser,
    board_syntax_validation::{check_flags, fixed_words, optional_text, word},
};
use crate::{
    board::board_grammar::BoardArguments,
    board::{
        board_ids::{EntryId, PlanId},
        board_protocol::{BoardOp, FeedbackMetadata, RecentCall},
        board_vocabulary::{EntryText, FeedbackImportKey, FeedbackKind, FeedbackState},
    },
};
use anyhow::Result;
use tp_core::build_identity::BuildIdentity;
use tp_core::coded_error::CodedContext;

pub(super) fn parse_feedback(
    options: &BoardArguments,
    payload: BoardTextPayload,
) -> Result<BoardCommand> {
    let verb = word(
        options,
        1,
        "feedback KIND SUMMARY | ls | triage E512 [NOTE] | close E512 STATE [NOTE]",
    )?;
    let op = match verb {
        "ls" => {
            check_flags(options, &["open", "after", "through"])?;
            fixed_words(
                options,
                2,
                2,
                "feedback ls [--open] [--after SEQ:E#] [--through SEQ]",
            )?;
            BoardOp::FeedbackList {
                open_only: options.board.open,
                after: board_read_parser::entry_after(options)?,
                through: board_read_parser::through(options)?,
                limit: board_read_parser::bounded_limit(options, 200)?,
            }
        }
        "triage" => {
            check_flags(options, &["text"])?;
            BoardOp::FeedbackTriage {
                entry: word(options, 2, "feedback triage E512 [NOTE]")?.parse::<EntryId>()?,
                note: optional_text(&payload.text)?,
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

fn feedback_metadata(
    options: &BoardArguments,
    steer_mode: Option<String>,
) -> Result<FeedbackMetadata> {
    let recent_calls = options
        .board
        .recent_calls
        .as_deref()
        .map(|value| {
            serde_json::from_str::<Vec<RecentCall>>(value).with_coded_context(|| {
                coded!(InvalidOptions, "--recent-calls must be a JSON call list")
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(FeedbackMetadata {
        version: BuildIdentity::current().version().into(),
        build_id: BuildIdentity::installed().map(|build| build.build_id().to_owned()),
        repo_key: None,
        cwd: if options.root.is_absolute() {
            String::new()
        } else {
            options.root.to_string_lossy().into_owned()
        },
        steer_mode,
        recent_calls,
    })
}
