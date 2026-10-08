//! Claim windows over live leases with stable entry cursors.
use rusqlite::{Connection, Row, params};

use super::NESTED_PAGE_LIMIT;
use crate::board::board_actor::claim_vendor;
use crate::board::board_domain::board_collections::{ClaimCursor, ClaimPage, ClaimView};
use crate::board::board_ids::{EntryId, EventSeq, PlanId, TaskId};
use crate::board::board_protocol::{BoardReply, BoardResult, ClaimRecord, ReadScope};
use crate::board::board_vocabulary::EntryText;
use crate::board::local_board::board_queries::collection_reads::{
    claim_ttl, count, sequence_window, validate_limit,
};
use crate::board::local_board::board_queries::read_scope_sql::ScopeSql;
use crate::board::local_board::{
    BoardError, WriteContext, actor_from_row, delegated_actor_from_row, invalid, require_plan,
    row_number, sql_error, sql_number,
};

#[allow(clippy::too_many_arguments)]
pub(in crate::board::local_board) fn claims_page(
    conn: &Connection,
    ctx: &WriteContext,
    plan: Option<PlanId>,
    own_stale: bool,
    scope: &ReadScope,
    after: Option<ClaimCursor>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    Ok(BoardReply::new(
        "local",
        BoardResult::Claims(claim_window(
            conn, ctx, plan, own_stale, scope, after, through, limit,
        )?),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::board::local_board) fn claim_window(
    conn: &Connection,
    ctx: &WriteContext,
    plan: Option<PlanId>,
    own_stale: bool,
    scope: &ReadScope,
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
    let scope_sql = ScopeSql::new(scope)?;
    let membership = ScopeSql::plan_predicate("c.plan_id", 7, 8);
    let predicate = format!(
        concat!(
            "c.ended_at IS NULL AND (?1 IS NULL OR c.plan_id=?1) ",
            "AND (NOT ?2 OR (a.user=?3 AND a.host=?4 AND a.harness=?5 AND c.last_active<?6)) ",
            "AND {membership} ",
            "AND (c.entry_id>?9 OR (c.entry_id=?9 AND c.id>?11)) AND e.seq<=?10"
        ),
        membership = membership
    );
    let parameters = params![
        plan.map(|plan| sql_number(plan.get())),
        own_stale,
        ctx.actor.user,
        ctx.actor.host,
        ctx.actor.harness.as_str(),
        cutoff,
        scope_sql.kind,
        scope_sql.keys_json,
        sql_number(after.map_or(0, |cursor| cursor.entry.get())),
        sql_number(through.get()),
        sql_number(after.map_or(0, |cursor| cursor.claim))
    ];
    let total = count(
        conn,
        &format!(
            concat!(
                "SELECT count(*) FROM claims c JOIN actors a ON a.id=c.actor_id ",
                "JOIN entries e ON e.id=c.entry_id WHERE {predicate}"
            ),
            predicate = predicate
        ),
        parameters,
    )?;
    let mut statement = conn
        .prepare(&format!(
            concat!(
                "SELECT c.plan_id,c.task_ordinal,a.user,a.host,a.harness,a.session,c.entry_id,c.scope,",
                "c.claimed_at,c.last_active,e.model,e.effort,c.id,",
                "d.user,d.host,d.harness,d.session FROM claims c ",
                "JOIN actors a ON a.id=c.actor_id JOIN entries e ON e.id=c.entry_id ",
                "LEFT JOIN actors d ON d.id=c.delegated_by WHERE {predicate} ",
                "ORDER BY c.entry_id,c.id LIMIT ?12"
            ),
            predicate = predicate
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            plan.map(|plan| sql_number(plan.get())),
            own_stale,
            ctx.actor.user,
            ctx.actor.host,
            ctx.actor.harness.as_str(),
            cutoff,
            scope_sql.kind,
            scope_sql.keys_json,
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
        scope: scope.clone(),
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
    let actor = actor_from_row(row, 2).map_err(sql_error)?;
    let model: Option<String> = row.get(10).map_err(sql_error)?;
    Ok(ClaimRecord {
        task: TaskId::new(plan, row_number(row, 1).map_err(sql_error)?)
            .map_err(BoardError::from)?,
        vendor: claim_vendor(&actor.harness, model.as_deref()),
        actor,
        entry: EntryId::new(row_number(row, 6).map_err(sql_error)?).map_err(BoardError::from)?,
        scope: EntryText::new(row.get::<_, String>(7).map_err(sql_error)?)
            .map_err(BoardError::from)?,
        claimed_at: row.get(8).map_err(sql_error)?,
        last_active,
        ended_at: None,
        end_reason: None,
        stale: last_active < cutoff,
        model,
        effort: row.get(11).map_err(sql_error)?,
        delegated_by: delegated_actor_from_row(row, 13).map_err(sql_error)?,
    })
}
