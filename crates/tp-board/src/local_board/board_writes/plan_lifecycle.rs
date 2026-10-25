//! Explicit plan closeout without changing task acceptance states.

#[cfg(test)]
mod tests;

use super::super::{
    BoardError, EntryDraft, WriteContext, insert_entry, insert_event, invalid, require_plan,
    sql_error, sql_number,
};
use crate::board::board_ids::{EntryId, PlanId};
use crate::board::board_protocol::BoardErrorCode;
use crate::board::board_protocol::{
    AuthorityRole, BoardChange, BoardReply, BoardResult, PlanLifecycleState,
};
use crate::board::board_vocabulary::{EntryKind, EntryText};
use rusqlite::{Transaction, params};

pub(in crate::board::local_board) fn require_open(
    conn: &rusqlite::Connection,
    plan: PlanId,
) -> Result<(), BoardError> {
    require_plan(conn, plan)?;
    let state: String = conn
        .query_row(
            "SELECT lifecycle_state FROM plans WHERE id=?1",
            [sql_number(plan.get())],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    match PlanLifecycleState::parse(&state).map_err(BoardError::from)? {
        PlanLifecycleState::Open => Ok(()),
        PlanLifecycleState::Closed => Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{plan} is closed; reopen it before starting new work"),
        )),
    }
}

pub(in crate::board::local_board) fn close_plan(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    note: &EntryText,
) -> Result<BoardReply, BoardError> {
    set_lifecycle(
        tx,
        ctx,
        plan,
        note,
        PlanLifecycleState::Open,
        PlanLifecycleState::Closed,
    )
}

pub(in crate::board::local_board) fn reopen_plan(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    note: &EntryText,
) -> Result<BoardReply, BoardError> {
    set_lifecycle(
        tx,
        ctx,
        plan,
        note,
        PlanLifecycleState::Closed,
        PlanLifecycleState::Open,
    )
}

fn set_lifecycle(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    note: &EntryText,
    expected: PlanLifecycleState,
    next: PlanLifecycleState,
) -> Result<BoardReply, BoardError> {
    require_plan(tx, plan)?;
    let authority = crate::board::local_board::board_authority::authority_for(
        tx,
        &ctx.actor,
        Some(plan),
        AuthorityRole::Decide,
        ctx.now,
    )?
    .ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidActor,
            format!("{} lacks decide authority for {plan}", ctx.actor),
        )
    })?;
    let authorized = ctx.with_authority(authority);
    let stored: String = tx
        .query_row(
            "SELECT lifecycle_state FROM plans WHERE id=?1",
            [sql_number(plan.get())],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let stored = PlanLifecycleState::parse(&stored).map_err(BoardError::from)?;
    if stored != expected {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            match next {
                PlanLifecycleState::Closed => format!("{plan} is already closed"),
                PlanLifecycleState::Open => format!("{plan} is already open"),
            },
        ));
    }
    if super::super::board_queries::board_reads::task_lineage::plan_has_live_claims_or_workers(
        tx, plan,
    )? {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{plan} has a live claim or worker"),
        ));
    }
    if next == PlanLifecycleState::Closed && has_open_tasks(tx, plan)? {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{plan} has tasks that are neither done nor transferred"),
        ));
    }

    let updated = tx
        .execute(
            "UPDATE plans SET lifecycle_state=?2 WHERE id=?1 AND lifecycle_state=?3",
            params![sql_number(plan.get()), next.as_str(), expected.as_str()],
        )
        .map_err(sql_error)?;
    if updated != 1 {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{plan} lifecycle changed concurrently"),
        ));
    }
    let kind = match next {
        PlanLifecycleState::Closed => EntryKind::PlanClose,
        PlanLifecycleState::Open => EntryKind::PlanReopen,
    };
    let entry = insert_entry(
        tx,
        &authorized,
        &EntryDraft {
            plan_id: Some(plan),
            kind,
            body: note.as_str().to_owned(),
            to_whom: None,
            supersedes: None,
            repo_key: None,
            state: None,
        },
    )?;
    let summary = match next {
        PlanLifecycleState::Closed => format!("closed plan {plan}"),
        PlanLifecycleState::Open => format!("reopened plan {plan}"),
    };
    insert_event(
        tx,
        &authorized,
        Some(plan),
        kind,
        &entry.to_string(),
        None,
        &summary,
    )?;
    Ok(change_reply(&authorized, entry, plan))
}

fn has_open_tasks(conn: &rusqlite::Connection, plan: PlanId) -> Result<bool, BoardError> {
    conn.query_row(
        concat!(
            "SELECT EXISTS(SELECT 1 FROM tasks task ",
            "WHERE task.plan_id=?1 AND task.column_name<>'done' ",
            "AND NOT EXISTS(SELECT 1 FROM task_transfers transfer ",
            "WHERE transfer.source_plan=task.plan_id AND transfer.source_ordinal=task.ordinal))"
        ),
        [sql_number(plan.get())],
        |row| row.get(0),
    )
    .map_err(sql_error)
}

fn change_reply(ctx: &WriteContext, entry: EntryId, plan: PlanId) -> BoardReply {
    BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry,
            seq: ctx.seq,
            plan: Some(plan),
            revision: None,
            task: None,
            transferred_to: None,
            deduplicated: false,
        }),
    )
}
