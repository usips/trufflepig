//! Escaped cells keep user-authored text inside controlled line-format grammar.

use crate::board::board_actor::{BoardActor, BoardRecipient};
use crate::board::board_ids::PlanId;
use std::fmt::Write;

pub(in crate::board::board_render) fn author(
    actor: &BoardActor,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    format!(
        "{}({}/{})",
        cell(&actor.identity()),
        cell(model.unwrap_or("unclaimed")),
        cell(effort.unwrap_or("unclaimed"))
    )
}

pub(in crate::board::board_render) fn recipient(to: &Option<BoardRecipient>) -> String {
    to.as_ref()
        .map_or_else(String::new, |to| format!(" (to {})", cell(to.as_str())))
}

pub(in crate::board::board_render) fn footer(
    text: &mut String,
    backend: &str,
    plan: Option<PlanId>,
) {
    writeln!(text, "backend: {}", cell(backend)).unwrap();
    if let Some(plan) = plan {
        writeln!(text, "commit trailer: Plan: {plan}").unwrap();
    }
}

/// Keep user-authored text inside one controlled line, including terminal escapes.
pub(in crate::board::board_render) fn cell(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\\' => escaped.push_str("\\\\"),
            ch if ch.is_control() => {
                use std::fmt::Write;
                write!(escaped, "\\u{{{:x}}}", u32::from(ch))
                    .expect("writing a string cannot fail");
            }
            ch => escaped.push(ch),
        }
    }
    escaped
}
