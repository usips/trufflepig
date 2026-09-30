//! Which members a query searches, from its selectors and the home member.
use crate::{cli::Arguments, workspace::member_root::MemberRoot};
use anyhow::{Context, Result, ensure};

/// Members to search. Without a selector, only the home member is searched when
/// one exists (`implicit_home`); `search` widens to all members when home has no
/// hits, but never while home itself is warming or unavailable.
pub(super) struct Scope {
    pub members: Vec<usize>,
    pub words: Vec<String>,
    pub implicit_home: bool,
}

pub(super) fn scope(roots: &[MemberRoot], options: &Arguments) -> Result<Scope> {
    let mut selection = options.member.clone().map(|m| format!("in:{m}"));
    let mut words = Vec::with_capacity(options.words.len());
    for argument in &options.words {
        let mut remaining = String::new();
        for chunk in argument.split_inclusive(char::is_whitespace) {
            let word = chunk.trim_end();
            if word.starts_with("in:") || matches!(word, "ws:home" | "ws:all") {
                ensure!(
                    selection.as_ref().is_none_or(|old| old == word),
                    "invalid_scope: conflicting workspace selectors"
                );
                selection = Some(word.to_owned());
            } else {
                remaining.push_str(chunk);
            }
        }
        if !remaining.trim().is_empty() {
            words.push(remaining.trim_end().to_owned());
        }
    }
    let home = roots
        .iter()
        .find(|root| root.is_home)
        .map(|root| root.name());
    let implicit_home = selection.is_none() && home.is_some();
    let selected = match selection.as_deref() {
        Some("ws:home") => Some(home.context("member_required: ws:home has no home member")?),
        Some("ws:all") => None,
        None => home,
        Some(s) => Some(s.strip_prefix("in:").expect("validated selector")),
    };
    if let Some(name) = selected {
        ensure!(
            roots.iter().any(|m| m.name() == name),
            "invalid_member: member is not configured"
        );
    }
    let mut members: Vec<_> = roots
        .iter()
        .enumerate()
        .filter(|(_, m)| selected.is_none_or(|n| m.name() == n))
        .map(|(i, _)| i)
        .collect();
    members.sort_by_key(|&i| (Some(roots[i].name()) != home, roots[i].name()));
    Ok(Scope {
        members,
        words,
        implicit_home,
    })
}

/// Scope of an unselected query after the home member ran, and whether to widen:
/// `home`, `home (ws:all adds N members)`, `all (no home hits)` (widens), or,
/// when home did not answer, `home (STATE; ws:all searches N members)`.
pub(super) fn implicit_home_scope(
    others: usize,
    home_answered: bool,
    home_hits: bool,
    home_state: &str,
) -> (String, bool) {
    let plural = if others == 1 { "" } else { "s" };
    if others == 0 {
        ("home".to_owned(), false)
    } else if !home_answered {
        let state = home_state.replace('_', " ");
        (
            format!("home ({state}; ws:all searches {others} member{plural})"),
            false,
        )
    } else if !home_hits {
        ("all (no home hits)".to_owned(), true)
    } else {
        (format!("home (ws:all adds {others} member{plural})"), false)
    }
}
