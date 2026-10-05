//! Render routes: plan markup, revision diffs, and proposal diffs.
use super::super::{
    BoardWebState, plan_markup,
    web_ops::{self, WebRequest},
};
use super::invalid;
use crate::board::{
    board_ids::{BoardRef, PlanRevision},
    board_protocol::{BOARD_API, BoardError, BoardOp, BoardReply, BoardResult},
    review_packet::build_ssot_diff,
};
use std::time::Instant;

/// Percent-decode a render-route suffix: only `%40` decodes, to the `@` of a
/// revision target; strict BoardRef parsing rejects every other byte.
pub(super) fn decode_render_target(raw: &str) -> Result<String, BoardError> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if bytes.get(index..index + 3) != Some(b"%40") {
            return Err(invalid("malformed percent encoding in render target"));
        }
        decoded.push(b'@');
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| invalid("render target is not valid UTF-8"))
}

pub(super) fn render_plan(
    state: &BoardWebState,
    target: &str,
    expires: Instant,
) -> Result<serde_json::Value, BoardError> {
    let target = BoardRef::parse(target).map_err(BoardError::from)?;
    if !matches!(target, BoardRef::Plan(_) | BoardRef::Revision(_)) {
        return Err(invalid("render plan requires P# or P#@#"));
    }
    let reply = web_ops::execute(
        &state.store,
        WebRequest {
            api: BOARD_API,
            op: BoardOp::Show { target },
        },
        expires,
    )?;
    let revision = match &reply.result {
        BoardResult::Plan(view) => &view.revision,
        BoardResult::Revision(revision) => revision,
        _ => return Err(invalid("plan rendering returned no revision")),
    };
    let rendered = plan_markup::render(revision.body.as_str());
    let mut value = serde_json::to_value(rendered).map_err(|error| invalid(error.to_string()))?;
    value["api"] = serde_json::json!(BOARD_API);
    value["revision"] = serde_json::json!(revision.id);
    value["snapshot_seq"] = serde_json::json!(reply.snapshot_seq);
    Ok(value)
}

pub(super) fn render_diff(
    state: &BoardWebState,
    target: &str,
    expires: Instant,
) -> Result<serde_json::Value, BoardError> {
    let target = BoardRef::parse(target).map_err(BoardError::from)?;
    if !matches!(target, BoardRef::Span(span) if span.end.is_some()) {
        return Err(invalid("render diff requires P#@#..#"));
    }
    let reply = web_ops::execute(
        &state.store,
        WebRequest {
            api: BOARD_API,
            op: BoardOp::Show { target },
        },
        expires,
    )?;
    let BoardResult::Diff(diff) = &reply.result else {
        return Err(invalid("diff rendering returned no revisions"));
    };
    let ssot = build_ssot_diff(&diff.before, &diff.after);
    Ok(
        serde_json::json!({ "api": BOARD_API, "before": ssot.before, "after": ssot.after,
        "hunks": ssot.hunks, "snapshot_seq": reply.snapshot_seq }),
    )
}

pub(super) fn render_proposal(
    state: &BoardWebState,
    target: &str,
    expires: Instant,
) -> Result<serde_json::Value, BoardError> {
    proposal_diff(target, |target| {
        web_ops::execute(
            &state.store,
            WebRequest {
                api: BOARD_API,
                op: BoardOp::Show { target },
            },
            expires,
        )
    })
}

pub(super) fn proposal_diff(
    target: &str,
    mut read: impl FnMut(BoardRef) -> Result<BoardReply, BoardError>,
) -> Result<serde_json::Value, BoardError> {
    let target = BoardRef::parse(target).map_err(BoardError::from)?;
    if !matches!(target, BoardRef::Entry(_)) {
        return Err(invalid("render proposal requires E#"));
    }
    let entry = read(target)?;
    let BoardResult::Entry(view) = &entry.result else {
        return Err(invalid("proposal rendering returned no entry"));
    };
    let proposal = view
        .proposal
        .as_ref()
        .ok_or_else(|| invalid("entry is not a proposal"))?;
    let base_id =
        PlanRevision::new(proposal.plan, proposal.base_revision).map_err(BoardError::from)?;
    let base = read(BoardRef::Revision(base_id))?;
    let BoardResult::Revision(before) = &base.result else {
        return Err(invalid("proposal base revision is unavailable"));
    };
    let mut proposed = before.clone();
    proposed.body = proposal.body.clone();
    let diff = build_ssot_diff(before, &proposed);
    let snapshot_seq = entry
        .snapshot_seq
        .zip(base.snapshot_seq)
        .map(|(entry, base)| entry.min(base));
    Ok(
        serde_json::json!({ "api": BOARD_API, "entry": proposal.entry, "before": before.id,
        "hunks": diff.hunks, "snapshot_seq": snapshot_seq }),
    )
}
