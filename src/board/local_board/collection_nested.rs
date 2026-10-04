//! Task ordinal ceilings and claim source sequences bound current-state collection pages.

#[cfg(test)]
mod tests;

use rusqlite::{Connection, Row, params};

use super::collection_reads::{claim_ttl, count, sequence_window, validate_limit};
use super::{
    BoardError, WriteContext, actor_from_row, invalid, require_plan, row_number, sql_error,
    sql_number,
};
use crate::board::board_actor::BoardRecipient;
use crate::board::board_domain::board_collections::{
    ClaimCursor, ClaimPage, ClaimView, TaskCeiling, TaskPage,
};
use crate::board::board_ids::{EntryId, EventSeq, PlanId, RepoKey, TaskId};
use crate::board::board_protocol::{BoardReply, BoardResult, ClaimRecord, TaskRecord};
use crate::board::board_vocabulary::{EntryText, PlanTitle};

const NESTED_PAGE_LIMIT: usize = 200;

pub(super) fn tasks_page(
    conn: &Connection,
    plan: PlanId,
    after: Option<TaskId>,
    ceiling: Option<TaskCeiling>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    if ceiling.is_none() && (after.is_some() || through.is_some()) {
        return Err(invalid(
            "invalid_options",
            "task continuation requires its captured ceiling",
        ));
    }
    Ok(BoardReply::new(
        "local",
        BoardResult::Tasks(task_window(conn, plan, after, ceiling, through, limit)?),
    ))
}

pub(super) fn task_window(
    conn: &Connection,
    plan: PlanId,
    after: Option<TaskId>,
    ceiling: Option<TaskCeiling>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<TaskPage, BoardError> {
    validate_limit(limit, NESTED_PAGE_LIMIT)?;
    if let Some(after) = after {
        after.validate().map_err(BoardError::from)?;
        if after.plan != plan {
            return Err(invalid(
                "invalid_reference",
                "task cursor belongs to a different plan",
            ));
        }
    }
    if let Some(ceiling) = ceiling {
        ceiling.validate().map_err(BoardError::from)?;
        if ceiling.plan != plan {
            return Err(invalid(
                "invalid_reference",
                "task ceiling belongs to a different plan",
            ));
        }
    }
    require_plan(conn, plan)?;
    let ceiling = match ceiling {
        Some(ceiling) => ceiling,
        None => {
            let ordinal = conn
                .query_row(
                    "SELECT coalesce(max(ordinal),0) FROM tasks WHERE plan_id=?1",
                    [sql_number(plan.get())],
                    |row| row_number(row, 0),
                )
                .map_err(sql_error)?;
            TaskCeiling { plan, ordinal }
        }
    };
    let (_, through) = sequence_window(conn, None, through)?;
    let parameters = params![
        sql_number(plan.get()),
        sql_number(after.map_or(0, |task| task.ordinal)),
        sql_number(ceiling.ordinal)
    ];
    let total = count(
        conn,
        "SELECT count(*) FROM tasks WHERE plan_id=?1 AND ordinal>?2 AND ordinal<=?3",
        parameters,
    )?;
    let mut statement = conn.prepare(
        "SELECT ordinal,title,column_name,assignee,section,seq FROM tasks WHERE plan_id=?1 AND ordinal>?2 AND ordinal<=?3 ORDER BY ordinal LIMIT ?4"
    ).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(plan.get()),
            sql_number(after.map_or(0, |task| task.ordinal)),
            sql_number(ceiling.ordinal),
            sql_number((limit + 1) as u64)
        ])
        .map_err(sql_error)?;
    let mut tasks = Vec::with_capacity((limit + 1).min(total));
    while let Some(row) = rows.next().map_err(sql_error)? {
        let assignee: Option<String> = row.get(3).map_err(sql_error)?;
        tasks.push(TaskRecord {
            id: TaskId::new(plan, row_number(row, 0).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            title: PlanTitle::new(row.get::<_, String>(1).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            column: row
                .get::<_, String>(2)
                .map_err(sql_error)?
                .parse()
                .map_err(BoardError::from)?,
            assignee: assignee
                .as_deref()
                .map(BoardRecipient::parse)
                .transpose()
                .map_err(BoardError::from)?,
            section: row.get(4).map_err(sql_error)?,
            seq: EventSeq::new(row_number(row, 5).map_err(sql_error)?),
        });
    }
    let next_after = if tasks.len() > limit {
        tasks.truncate(limit);
        tasks.last().map(|task| task.id)
    } else {
        None
    };
    Ok(TaskPage {
        plan,
        omitted: total.saturating_sub(tasks.len()),
        tasks,
        after,
        ceiling,
        through,
        next_after,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn claims_page(
    conn: &Connection,
    ctx: &WriteContext,
    plan: Option<PlanId>,
    own_stale: bool,
    repo_key: Option<&RepoKey>,
    all: bool,
    after: Option<ClaimCursor>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    Ok(BoardReply::new(
        "local",
        BoardResult::Claims(claim_window(
            conn, ctx, plan, own_stale, repo_key, all, after, through, limit,
        )?),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn claim_window(
    conn: &Connection,
    ctx: &WriteContext,
    plan: Option<PlanId>,
    own_stale: bool,
    repo_key: Option<&RepoKey>,
    all: bool,
    after: Option<ClaimCursor>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<ClaimPage, BoardError> {
    validate_limit(limit, NESTED_PAGE_LIMIT)?;
    if let Some(plan) = plan {
        require_plan(conn, plan)?;
    }
    if !own_stale && plan.is_none() {
        return Err(invalid(
            "invalid_options",
            "claims require a plan unless own_stale is true",
        ));
    }
    if let Some(after) = after {
        after.validate().map_err(BoardError::from)?;
    }
    let (_, through) = sequence_window(conn, None, through)?;
    let cutoff = ctx.now.saturating_sub(ctx.claim_ttl_secs.max(0));
    let predicate = "c.ended_at IS NULL AND (?1 IS NULL OR c.plan_id=?1) AND (NOT ?2 OR (a.user=?3 AND a.host=?4 AND a.harness=?5 AND a.session=?6 AND c.last_active<?7)) AND (NOT ?2 OR ?8 OR NOT EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=c.plan_id) OR EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=c.plan_id AND scope.repo_key=?9)) AND (c.entry_id>?10 OR (c.entry_id=?10 AND c.id>?12)) AND e.seq<=?11";
    let parameters = params![
        plan.map(|plan| sql_number(plan.get())),
        own_stale,
        ctx.actor.user,
        ctx.actor.host,
        ctx.actor.harness.as_str(),
        ctx.actor.session,
        cutoff,
        all,
        repo_key.map(RepoKey::as_str),
        sql_number(after.map_or(0, |cursor| cursor.entry.get())),
        sql_number(through.get()),
        sql_number(after.map_or(0, |cursor| cursor.claim))
    ];
    let total = count(
        conn,
        &format!(
            "SELECT count(*) FROM claims c JOIN actors a ON a.id=c.actor_id JOIN entries e ON e.id=c.entry_id WHERE {predicate}"
        ),
        parameters,
    )?;
    let mut statement = conn.prepare(&format!(
        "SELECT c.plan_id,c.task_ordinal,a.user,a.host,a.harness,a.session,c.entry_id,c.scope,c.claimed_at,c.last_active,e.model,e.effort,c.id FROM claims c JOIN actors a ON a.id=c.actor_id JOIN entries e ON e.id=c.entry_id WHERE {predicate} ORDER BY c.entry_id,c.id LIMIT ?13"
    )).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            plan.map(|plan| sql_number(plan.get())),
            own_stale,
            ctx.actor.user,
            ctx.actor.host,
            ctx.actor.harness.as_str(),
            ctx.actor.session,
            cutoff,
            all,
            repo_key.map(RepoKey::as_str),
            sql_number(after.map_or(0, |cursor| cursor.entry.get())),
            sql_number(through.get()),
            sql_number(after.map_or(0, |cursor| cursor.claim)),
            sql_number((limit + 1) as u64)
        ])
        .map_err(sql_error)?;
    let mut claims = Vec::with_capacity((limit + 1).min(total));
    while let Some(row) = rows.next().map_err(sql_error)? {
        let claim = active_claim_row(row, cutoff)?;
        let cursor = ClaimCursor {
            entry: claim.entry,
            claim: row_number(row, 12).map_err(sql_error)?,
        };
        cursor.validate().map_err(BoardError::from)?;
        claims.push(ClaimView { claim, cursor });
    }
    let next_after = if claims.len() > limit {
        claims.truncate(limit);
        claims.last().map(|claim| claim.cursor)
    } else {
        None
    };
    Ok(ClaimPage {
        plan,
        own_stale,
        repo_key: repo_key.cloned(),
        all,
        omitted: total.saturating_sub(claims.len()),
        claims,
        after,
        through,
        next_after,
        server_now: ctx.now,
        claim_ttl_secs: claim_ttl(ctx),
    })
}

fn active_claim_row(row: &Row<'_>, cutoff: i64) -> Result<ClaimRecord, BoardError> {
    let plan = PlanId::new(row_number(row, 0).map_err(sql_error)?).map_err(BoardError::from)?;
    let last_active: i64 = row.get(9).map_err(sql_error)?;
    Ok(ClaimRecord {
        task: TaskId::new(plan, row_number(row, 1).map_err(sql_error)?)
            .map_err(BoardError::from)?,
        actor: actor_from_row(row, 2).map_err(sql_error)?,
        entry: EntryId::new(row_number(row, 6).map_err(sql_error)?).map_err(BoardError::from)?,
        scope: EntryText::new(row.get::<_, String>(7).map_err(sql_error)?)
            .map_err(BoardError::from)?,
        claimed_at: row.get(8).map_err(sql_error)?,
        last_active,
        ended_at: None,
        end_reason: None,
        stale: last_active < cutoff,
        model: row.get(10).map_err(sql_error)?,
        effort: row.get(11).map_err(sql_error)?,
    })
}
