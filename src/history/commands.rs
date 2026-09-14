mod change_entries;
use change_entries::entries;
mod historical_render;
mod historical_symbols;
use historical_symbols::{symbol_correspondence, symbol_entries};

use super::*;
use super::{
    comparison::{self, FileChange},
    targets::{Selection, Target},
};
use crate::{
    identity::ByteSpan,
    results::{self, ResultEntry},
};
use anyhow::Context;
use serde_json::json;

pub(super) fn select(
    store: &mut Store,
    target: Option<&str>,
    budget: &OutputBudget,
) -> Result<std::result::Result<Option<Target>, String>> {
    match target.map(|t| targets::resolve(store, t)).transpose()? {
        Some(Selection::Candidates(hits)) => {
            let id = results::save_entries(
                store,
                store.generation()?,
                json!({"outcome":"ambiguous_symbol","selection_required":true}),
                hits.into_iter().map(ResultEntry::LiveSource).collect(),
                false,
            )?;
            Ok(Err(results::page(store, &id, 0, 20, budget)?))
        }
        Some(Selection::Target(target)) => Ok(Ok(Some(target))),
        None => Ok(Ok(None)),
    }
}

pub(super) fn selected(change: &FileChange, target: Option<&Target>) -> bool {
    target.is_none_or(|t| {
        change.before.as_ref().is_some_and(|f| f.path == t.path)
            || change.after.as_ref().is_some_and(|f| f.path == t.path)
    })
}

pub(super) fn since(
    history: &History,
    store: &mut Store,
    revision: &str,
    target: Option<&str>,
    uncommitted: bool,
    budget: &OutputBudget,
) -> Result<String> {
    let before = history.repository.resolve(revision)?;
    if uncommitted {
        return working_tree::since_uncommitted(history, store, &before, target, budget);
    }
    let target = match select(store, target, budget)? {
        Ok(target) => target,
        Err(response) => return Ok(response),
    };
    let changes = comparison::compare(history, Some(&before), &history.tip)?;
    let (entries, excluded, entry_truncated) = entries(
        history,
        Some(&before),
        &history.tip,
        &changes,
        target.as_ref(),
    )?;
    emit(
        store,
        entries,
        json!({"operation":"since","before":before,"after":history.tip,"endpoint":"captured_head","comparison":"direct_net","excluded":excluded}),
        entry_truncated,
        budget,
    )
}

pub(super) fn diff(
    history: &History,
    store: &mut Store,
    revision: &str,
    target: Option<&str>,
    budget: &OutputBudget,
) -> Result<String> {
    let after = history.repository.resolve(revision)?;
    let before = first_parent(history, &after)?;
    let target = match select(store, target, budget)? {
        Ok(target) => target,
        Err(response) => return Ok(response),
    };
    let changes = comparison::compare(history, before.as_ref(), &after)?;
    let (entries, excluded, entry_truncated) =
        entries(history, before.as_ref(), &after, &changes, target.as_ref())?;
    let response = emit(
        store,
        entries,
        json!({"operation":"diff","before":before,"after":after,"traversal":"first_parent","excluded":excluded,"source":if target.is_some(){"scoped_hunks"}else{"explicit_show_required"}}),
        entry_truncated,
        budget,
    )?;
    if target.is_none() {
        return Ok(response);
    }
    historical_render::scoped_hunks(history, response, budget)
}

pub(super) fn hist(
    history: &History,
    store: &mut Store,
    target: &str,
    budget: &OutputBudget,
) -> Result<String> {
    let mut target = match select(store, Some(target), budget)? {
        Ok(target) => target.context("missing target")?,
        Err(response) => return Ok(response),
    };
    let start_depth = if let Some(commit) = target.commit {
        use rusqlite::OptionalExtension;
        history
            .conn
            .query_row(
                "SELECT depth FROM traversal WHERE tip=?1 AND oid=?2",
                rusqlite::params![history.tip.as_str(), commit.as_str()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .context("history_unavailable: selected commit is outside captured first-parent view")?
    } else {
        0
    };
    let mut statement=history.conn.prepare("SELECT c.oid,c.parent,t.eligible FROM traversal t JOIN commits c ON c.oid=t.oid WHERE t.tip=?1 AND t.depth>=?2 ORDER BY t.depth")?;
    let commits = statement
        .query_map(rusqlite::params![history.tip.as_str(), start_depth], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut collected = Vec::with_capacity(commits.len().min(256));
    let mut excluded = 0usize;
    let mut examined = 0usize;
    let mut scope_boundary = false;
    let mut entry_truncated = false;
    for (oid, parent, eligible) in commits.iter().take(256) {
        examined += 1;
        let after = GitOid::parse(oid)?;
        let before = parent.as_deref().map(GitOid::parse).transpose()?;
        let changes = comparison::compare(history, before.as_ref(), &after)?;
        let (mut changes_found, failed, batch_truncated) =
            entries(history, before.as_ref(), &after, &changes, Some(&target))?;
        excluded += failed;
        entry_truncated |= batch_truncated;
        let mut reached_addition = false;
        if target.symbol.is_some() {
            for change in &changes {
                if !selected(change, Some(&target)) {
                    continue;
                }
                let entry = comparison::entry(history, before.as_ref(), &after, change)?;
                let (_, span, revision) = symbol_correspondence(history, entry, &target)?;
                if let Some(span) = span {
                    target.span = Some(span);
                    target.revision = revision;
                } else {
                    reached_addition = true;
                }
            }
        }
        for change in &changes {
            if change.status == "renamed_exact_blob"
                && change.after.as_ref().is_some_and(|f| f.path == target.path)
            {
                target.path = change
                    .before
                    .as_ref()
                    .expect("rename preimage")
                    .path
                    .clone();
            }
        }
        scope_boundary |= changes
            .iter()
            .any(|c| selected(c, Some(&target)) && c.status.starts_with("scope_boundary"));
        if *eligible {
            entry_truncated |=
                changes_found.len() > results::MAX_HITS.saturating_sub(collected.len());
            changes_found.truncate(results::MAX_HITS.saturating_sub(collected.len()));
            collected.append(&mut changes_found);
        }
        if reached_addition || scope_boundary || collected.len() >= results::MAX_HITS {
            break;
        }
    }
    let mut coverage = history.status()?;
    coverage["operation"] = json!("hist");
    coverage["examined"] = json!(examined);
    coverage["excluded"] = json!(excluded);
    coverage["scope_boundary"] = json!(scope_boundary);
    emit(
        store,
        collected,
        coverage,
        entry_truncated || examined < commits.len(),
        budget,
    )
}

pub(super) fn first_parent(history: &History, revision: &GitOid) -> Result<Option<GitOid>> {
    let output = history
        .repository
        .run(&["cat-file", "commit", revision.as_str()])?;
    for line in output
        .split(|&b| b == b'\n')
        .take_while(|line| !line.is_empty())
    {
        if let Some(parent) = line.strip_prefix(b"parent ") {
            return Ok(Some(GitOid::parse(std::str::from_utf8(parent)?)?));
        }
    }
    Ok(None)
}

fn emit(
    store: &mut Store,
    entries: Vec<ResultEntry>,
    coverage: Value,
    truncated: bool,
    budget: &OutputBudget,
) -> Result<String> {
    let id = results::save_entries(store, store.generation()?, coverage, entries, truncated)?;
    results::page(store, &id, 0, 20, budget)
}

fn ensure_diff_capacity(before: &[u8], after: &[u8]) -> Result<()> {
    anyhow::ensure!(
        before.iter().filter(|&&b| b == b'\n').count()
            + after.iter().filter(|&&b| b == b'\n').count()
            <= 100_000,
        "history_resource_limited: diff line-token staging capacity reached"
    );
    Ok(())
}
