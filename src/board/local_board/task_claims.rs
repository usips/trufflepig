//! Exclusive task leases, ordinary activity, and durable claim history.

#[cfg(test)]
mod tests;

use rusqlite::{Connection, Row, Transaction, params};

use super::{
    BoardError, EntryDraft, WriteContext, actor_from_row, can_accept, insert_entry, insert_event,
    invalid, require_plan, row_number, sql_error, sql_number,
};
use crate::board::board_actor::BoardRecipient;
use crate::board::board_ids::{EntryId, EventSeq, PlanId, TaskId};
use crate::board::board_protocol::{
    BoardChange, BoardReply, BoardResult, ClaimEndReason, ClaimRecord, CommitCoauthor, TaskRecord,
};
use crate::board::board_vocabulary::{EntryKind, EntryText, PlanTitle, TaskColumn};

struct StoredClaim {
    actor_id: i64,
    record: ClaimRecord,
}

pub(super) fn create_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    title: &PlanTitle,
    to: Option<&BoardRecipient>,
    section: Option<&str>,
) -> Result<BoardReply, BoardError> {
    require_plan(tx, plan)?;
    let task = allocate_task(tx, ctx, plan, title, to, section)?;
    let summary = format!("created {task}: {}", title.as_str());
    let entry = task_entry(tx, ctx, task, summary.clone(), to)?;
    insert_event(
        tx,
        ctx,
        Some(plan),
        "task",
        &task.to_string(),
        to.map(BoardRecipient::as_str),
        &summary,
    )?;
    change(ctx, entry, task)
}

pub(super) fn carve_claim(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    title: &PlanTitle,
    scope: &EntryText,
    section: Option<&str>,
) -> Result<BoardReply, BoardError> {
    require_plan(tx, plan)?;
    let task = allocate_task(tx, ctx, plan, title, None, section)?;
    claim_task(tx, ctx, task, scope)
}

fn allocate_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    title: &PlanTitle,
    to: Option<&BoardRecipient>,
    section: Option<&str>,
) -> Result<TaskId, BoardError> {
    let ordinal: u64 = tx
        .query_row(
            "UPDATE plans SET next_task=next_task+1 WHERE id=?1 RETURNING next_task",
            [sql_number(plan.get())],
            |row| row_number(row, 0),
        )
        .map_err(sql_error)?;
    let task = TaskId::new(plan, ordinal).map_err(BoardError::from)?;
    tx.execute(
        "INSERT INTO tasks(plan_id,ordinal,title,column_name,assignee,section,seq) VALUES(?1,?2,?3,'todo',?4,?5,?6)",
        params![sql_number(plan.get()), sql_number(ordinal), title.as_str(), to.map(BoardRecipient::as_str), section, sql_number(ctx.seq.get())],
    ).map_err(sql_error)?;
    Ok(task)
}

pub(super) fn claim_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    scope: &EntryText,
) -> Result<BoardReply, BoardError> {
    require_task(tx, task)?;
    let holder = active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?;
    if let Some(holder) = &holder {
        if holder.actor_id != ctx.actor_id && !holder.record.stale {
            let claim = &holder.record;
            return Err(invalid(
                "claim_conflict",
                format!(
                    "{task} held by {} ({}/{}) since {}, active {}s ago (last activity {})",
                    claim.actor,
                    claim.model.as_deref().unwrap_or("unknown"),
                    claim.effort.as_deref().unwrap_or("unknown"),
                    claim.claimed_at,
                    ctx.now.saturating_sub(claim.last_active),
                    claim.last_active,
                ),
            ));
        }
        let reason = if holder.actor_id == ctx.actor_id {
            "released"
        } else {
            "taken_over"
        };
        end_claim(tx, task, ctx.now, reason)?;
    }
    let entry = insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: Some(task.plan),
            kind: EntryKind::Claim,
            body: scope.as_str().to_owned(),
            to_whom: None,
            supersedes: None,
            repo_key: None,
            state: None,
            dedupe_key: None,
        },
    )?;
    tx.execute(
        "INSERT INTO claims(plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active) VALUES(?1,?2,?3,?4,?5,?6,?6)",
        params![sql_number(task.plan.get()), sql_number(task.ordinal), ctx.actor_id, sql_number(entry.get()), scope.as_str(), ctx.now],
    ).map_err(sql_error)?;
    let assignee = BoardRecipient::for_actor(&ctx.actor);
    tx.execute(
        "UPDATE tasks SET column_name='doing',assignee=?3,seq=?4 WHERE plan_id=?1 AND ordinal=?2",
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            assignee.as_str(),
            sql_number(ctx.seq.get())
        ],
    )
    .map_err(sql_error)?;
    let previous = holder.filter(|claim| claim.actor_id != ctx.actor_id);
    let recipient = previous
        .as_ref()
        .map(|claim| BoardRecipient::for_actor(&claim.record.actor));
    let preview_end = scope
        .as_str()
        .char_indices()
        .map(|(offset, _)| offset)
        .take_while(|offset| *offset <= 2048)
        .last()
        .unwrap_or(0);
    let preview = if scope.as_str().len() > 2048 {
        &scope.as_str()[..preview_end]
    } else {
        scope.as_str()
    };
    let summary = match previous {
        Some(claim) => format!(
            "took {task}: {preview}; took over stale claim from {}",
            claim.record.actor
        ),
        None => format!("took {task}: {preview}"),
    };
    insert_event(
        tx,
        ctx,
        Some(task.plan),
        "claim",
        &entry.to_string(),
        recipient.as_ref().map(BoardRecipient::as_str),
        &summary,
    )?;
    change(ctx, entry, task)
}

pub(super) fn move_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    column: TaskColumn,
    to: Option<&BoardRecipient>,
) -> Result<BoardReply, BoardError> {
    require_task(tx, task)?;
    let holder = active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?;
    let reassign = column == TaskColumn::Doing && to.is_some();
    let privileged = can_accept(tx, &ctx.actor, task.plan)?;
    if reassign && !privileged {
        return Err(invalid(
            "invalid_actor",
            "task reassignment requires the plan owner's human or steward identity",
        ));
    }
    if holder
        .as_ref()
        .is_some_and(|claim| claim.actor_id != ctx.actor_id)
        && !privileged
    {
        return Err(invalid(
            "invalid_actor",
            format!("cannot move {task} held by another actor"),
        ));
    }
    let recipient = to.cloned().or_else(|| {
        holder
            .as_ref()
            .map(|claim| BoardRecipient::for_actor(&claim.record.actor))
    });
    if reassign || column != TaskColumn::Doing {
        end_claim(
            tx,
            task,
            ctx.now,
            if reassign { "reassigned" } else { "released" },
        )?;
    }
    tx.execute(
        "UPDATE tasks SET column_name=?3,assignee=coalesce(?4,assignee),seq=?5 WHERE plan_id=?1 AND ordinal=?2",
        params![sql_number(task.plan.get()), sql_number(task.ordinal), column.as_str(), to.map(BoardRecipient::as_str), sql_number(ctx.seq.get())],
    ).map_err(sql_error)?;
    let mut summary = format!("{task} -> {column}");
    if let Some(to) = to {
        summary.push_str(&format!(" (to {to})"));
    }
    if reassign {
        summary.push_str("; assigned, awaiting recipient claim");
        if let Some(holder) = holder {
            summary.push_str(&format!("; reassigned from {}", holder.record.actor));
        }
    }
    let entry = task_entry(tx, ctx, task, summary.clone(), recipient.as_ref())?;
    insert_event(
        tx,
        ctx,
        Some(task.plan),
        "task",
        &task.to_string(),
        recipient.as_ref().map(BoardRecipient::as_str),
        &summary,
    )?;
    change(ctx, entry, task)
}

fn require_task(conn: &Connection, task: TaskId) -> Result<(), BoardError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if exists {
        Ok(())
    } else {
        Err(invalid("invalid_reference", format!("unknown task {task}")))
    }
}

fn end_claim(tx: &Transaction<'_>, task: TaskId, now: i64, reason: &str) -> Result<(), BoardError> {
    tx.execute(
        "UPDATE claims SET ended_at=?3,end_reason=?4 WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
        params![sql_number(task.plan.get()), sql_number(task.ordinal), now, reason],
    ).map_err(sql_error)?;
    Ok(())
}

fn task_entry(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    body: String,
    to: Option<&BoardRecipient>,
) -> Result<EntryId, BoardError> {
    insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: Some(task.plan),
            kind: EntryKind::Task,
            body,
            to_whom: to.map(|to| to.as_str().to_owned()),
            supersedes: None,
            repo_key: None,
            state: None,
            dedupe_key: None,
        },
    )
}

fn change(ctx: &WriteContext, entry: EntryId, task: TaskId) -> Result<BoardReply, BoardError> {
    Ok(BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry,
            seq: ctx.seq,
            plan: Some(task.plan),
            revision: None,
            task: Some(task),
            deduplicated: false,
        }),
    ))
}

pub(super) fn refresh_plan_claims(
    tx: &Transaction<'_>,
    actor_id: i64,
    plan: PlanId,
    now: i64,
) -> Result<(), BoardError> {
    tx.execute(
        "UPDATE claims SET last_active=max(last_active,?3) WHERE actor_id=?1 AND plan_id=?2 AND ended_at IS NULL",
        params![actor_id, sql_number(plan.get()), now],
    ).map_err(sql_error)?;
    Ok(())
}

pub(super) fn refresh_inbox_claims(
    tx: &Transaction<'_>,
    actor_id: i64,
    now: i64,
) -> Result<(), BoardError> {
    tx.execute(
        "UPDATE claims SET last_active=max(last_active,?2) WHERE actor_id=?1 AND ended_at IS NULL",
        params![actor_id, now],
    )
    .map_err(sql_error)?;
    Ok(())
}

pub(super) fn refresh_commit_claims(
    tx: &Transaction<'_>,
    plan: PlanId,
    ordinal: u64,
    coauthors: &[CommitCoauthor],
    committed_at: i64,
    now: i64,
) -> Result<(), BoardError> {
    if committed_at > now {
        return Ok(());
    }
    for coauthor in coauthors {
        tx.execute(
            "UPDATE claims SET last_active=max(last_active,?5) WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL AND claimed_at<=?4 AND actor_id IN (SELECT id FROM actors WHERE harness=?3)",
            params![sql_number(plan.get()), sql_number(ordinal), coauthor.harness.as_str(), committed_at, now],
        ).map_err(sql_error)?;
    }
    Ok(())
}

pub(super) fn read_tasks(conn: &Connection, plan: PlanId) -> Result<Vec<TaskRecord>, BoardError> {
    let mut statement = conn.prepare("SELECT ordinal,title,column_name,assignee,section,seq FROM tasks WHERE plan_id=?1 ORDER BY ordinal").map_err(sql_error)?;
    let mut rows = statement
        .query([sql_number(plan.get())])
        .map_err(sql_error)?;
    let mut tasks = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let title: String = row.get(1).map_err(sql_error)?;
        let column: String = row.get(2).map_err(sql_error)?;
        let assignee: Option<String> = row.get(3).map_err(sql_error)?;
        tasks.push(TaskRecord {
            id: TaskId::new(plan, row_number(row, 0).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            title: PlanTitle::new(title).map_err(BoardError::from)?,
            column: column.parse().map_err(BoardError::from)?,
            assignee: assignee
                .as_deref()
                .map(BoardRecipient::parse)
                .transpose()
                .map_err(BoardError::from)?,
            section: row.get(4).map_err(sql_error)?,
            seq: EventSeq::new(row_number(row, 5).map_err(sql_error)?),
        });
    }
    Ok(tasks)
}

const CLAIM_SELECT: &str = "SELECT c.actor_id,c.task_ordinal,a.user,a.host,a.harness,a.session,c.entry_id,c.scope,c.claimed_at,c.last_active,c.ended_at,c.end_reason,e.model,e.effort FROM claims c JOIN actors a ON a.id=c.actor_id JOIN entries e ON e.id=c.entry_id";

fn active_claim(
    conn: &Connection,
    task: TaskId,
    now: i64,
    ttl: i64,
) -> Result<Option<StoredClaim>, BoardError> {
    let sql =
        format!("{CLAIM_SELECT} WHERE c.plan_id=?1 AND c.task_ordinal=?2 AND c.ended_at IS NULL");
    let mut statement = conn.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal)
        ])
        .map_err(sql_error)?;
    rows.next()
        .map_err(sql_error)?
        .map(|row| claim_from_row(row, task.plan, now, ttl))
        .transpose()
}

pub(super) fn read_claims(
    conn: &Connection,
    plan: PlanId,
    now: i64,
    ttl: i64,
) -> Result<Vec<ClaimRecord>, BoardError> {
    read_claims_window(conn, plan, i64::MIN, i64::MAX, now, ttl)
}

pub(super) fn read_claims_window(
    conn: &Connection,
    plan: PlanId,
    start: i64,
    end: i64,
    now: i64,
    ttl: i64,
) -> Result<Vec<ClaimRecord>, BoardError> {
    let sql = format!(
        "{CLAIM_SELECT} WHERE c.plan_id=?1 AND c.claimed_at<=?3 AND (c.ended_at IS NULL OR c.ended_at>=?2) ORDER BY c.claimed_at,c.id"
    );
    let mut statement = conn.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![sql_number(plan.get()), start, end])
        .map_err(sql_error)?;
    let mut claims = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        claims.push(claim_from_row(row, plan, now, ttl)?.record);
    }
    Ok(claims)
}

fn claim_from_row(
    row: &Row<'_>,
    plan: PlanId,
    now: i64,
    ttl: i64,
) -> Result<StoredClaim, BoardError> {
    let scope: String = row.get(7).map_err(sql_error)?;
    let last_active: i64 = row.get(9).map_err(sql_error)?;
    let ended_at: Option<i64> = row.get(10).map_err(sql_error)?;
    let reason: Option<String> = row.get(11).map_err(sql_error)?;
    let end_reason = match reason.as_deref() {
        None => None,
        Some("released") => Some(ClaimEndReason::Released),
        Some("taken_over") => Some(ClaimEndReason::TakenOver),
        Some("reassigned") => Some(ClaimEndReason::Reassigned),
        Some(other) => {
            return Err(invalid(
                "board_unavailable",
                format!("invalid stored claim end reason {other}"),
            ));
        }
    };
    Ok(StoredClaim {
        actor_id: row.get(0).map_err(sql_error)?,
        record: ClaimRecord {
            task: TaskId::new(plan, row_number(row, 1).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            actor: actor_from_row(row, 2).map_err(sql_error)?,
            entry: EntryId::new(row_number(row, 6).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            scope: EntryText::new(scope).map_err(BoardError::from)?,
            claimed_at: row.get(8).map_err(sql_error)?,
            last_active,
            ended_at,
            end_reason,
            stale: ended_at.is_none() && last_active < now.saturating_sub(ttl.max(0)),
            model: row.get(12).map_err(sql_error)?,
            effort: row.get(13).map_err(sql_error)?,
        },
    })
}
