//! Session snapshots, append-only posts, and idempotent repository evidence.

use rusqlite::{Transaction, params};

use super::super::*;
use super::task_claims;

mod commit_link_writes;
mod repository_writes;

use crate::board::board_actor::BoardRecipient;
use crate::board::board_vocabulary::EntryText;
pub(in crate::board::local_board) use commit_link_writes::{link_commit, link_commits};
pub(in crate::board::local_board) use repository_writes::{forget_repo_path, record_scan, register_repo};

pub(in crate::board::local_board) fn hello(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    model: &str,
    effort: Option<&str>,
) -> Result<BoardReply, BoardError> {
    tx.execute(
        "UPDATE agent_sessions SET model=?2,effort=?3,last_seen=?4 WHERE actor_id=?1",
        params![ctx.actor_id, model, effort, ctx.now],
    )
    .map_err(sql_error)?;
    let snapshot = WriteContext {
        actor_id: ctx.actor_id,
        actor: ctx.actor.clone(),
        model: Some(model.to_owned()),
        effort: effort.map(str::to_owned),
        now: ctx.now,
        seq: ctx.seq,
        claim_ttl_secs: ctx.claim_ttl_secs,
        via: None,
    };
    let body = effort.map_or_else(|| model.to_owned(), |effort| format!("{model}/{effort}"));
    let entry = insert_entry(
        tx,
        &snapshot,
        &EntryDraft {
            plan_id: None,
            kind: EntryKind::Hello,
            body: body.clone(),
            to_whom: None,
            supersedes: None,
            repo_key: None,
            state: None,
        },
    )?;
    insert_event(
        tx,
        &snapshot,
        None,
        EntryKind::Hello,
        &entry.to_string(),
        None,
        &body,
    )?;
    let (cursor, first_seen, last_seen): (Option<i64>, i64, i64) = tx
        .query_row(
            "SELECT cursor_seq,first_seen,last_seen FROM agent_sessions WHERE actor_id=?1",
            [ctx.actor_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sql_error)?;
    Ok(BoardReply::new(
        "local",
        BoardResult::Session(SessionRecord {
            actor: ctx.actor.clone(),
            model: Some(model.to_owned()),
            effort: effort.map(str::to_owned),
            cursor: EventSeq::new(sqlite_u64(cursor.unwrap_or(0))?),
            first_seen,
            last_seen,
        }),
    ))
}

pub(in crate::board::local_board) fn post(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    target: &BoardRef,
    kind: EntryKind,
    body: &EntryText,
    to: Option<&BoardRecipient>,
    supersedes: Option<EntryId>,
) -> Result<BoardReply, BoardError> {
    let plan = target
        .plan_id()
        .ok_or_else(|| invalid("invalid_reference", "post requires a plan or task"))?;
    require_plan(tx, plan)?;
    if let BoardRef::Task(task) = target {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
                params![sql_number(plan.get()), sql_number(task.ordinal)],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if !exists {
            return Err(invalid("invalid_reference", format!("unknown task {task}")));
        }
    }
    if let Some(previous) = supersedes {
        let prior = read_entry(tx, previous)?;
        if prior.plan != Some(plan) {
            return Err(invalid(
                "invalid_reference",
                "superseded entry belongs to a different plan",
            ));
        }
    }
    let entry = insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: Some(plan),
            kind,
            body: body.as_str().to_owned(),
            to_whom: to.cloned(),
            supersedes,
            repo_key: None,
            state: None,
        },
    )?;
    if matches!(target, BoardRef::Task(_)) {
        tx.execute(
            "INSERT OR IGNORE INTO entry_refs(entry_id,target) VALUES(?1,?2)",
            params![sql_number(entry.get()), target.to_string()],
        )
        .map_err(sql_error)?;
    }
    insert_event(
        tx,
        ctx,
        Some(plan),
        kind,
        &entry.to_string(),
        to,
        body.as_str(),
    )?;
    Ok(ctx.change_reply(
        entry,
        Some(plan),
        None,
        if let BoardRef::Task(task) = target {
            Some(*task)
        } else {
            None
        },
    ))
}

#[cfg(test)]
mod tests;
