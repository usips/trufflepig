//! Open-reminder predicate and capped reminder totals.
use super::*;
use crate::board::local_board::board_queries::read_scope_sql::ScopeSql;

/// Reminder totals are exact up to this cap; larger backlogs report a lower bound.
const OPEN_REMINDER_COUNT_CAP: u64 = 200;

/// Open-reminder predicate over `entries e`: ?1 user, ?2 harness, ?3 identity,
/// ?4 scope kind, ?5 JSON keys, ?6 through. The kind prefix drives the scan from
/// `entries_kind_state`; authorship matches user and harness so a new session
/// of the same harness keeps its reminders; proposal currency is as of ?6.
pub(in crate::board::local_board::board_queries::board_reads) fn reminder_predicate() -> String {
    let strict_scope = ScopeSql::strict_plan_predicate("e.plan_id", 4, 5);
    format!(
        concat!(
            "e.kind IN ('question','proposal','feedback') ",
            "AND ((EXISTS(SELECT 1 FROM proposals p WHERE p.entry_id=e.id ",
            "AND p.state='open' AND (p.base_revision=(SELECT max(number) FROM revisions head_at ",
            "WHERE head_at.plan_id=p.plan_id AND head_at.seq<=?6) OR EXISTS(SELECT 1 FROM actors author ",
            "WHERE author.id=e.actor_id AND author.user=?1 AND author.harness=?2))) ",
            "OR (e.kind='feedback' AND e.state IN ('open','triaged')) OR ({OPEN_QUESTION}))) ",
            "AND (e.to_whom IS NULL OR e.to_whom IN (?1,?2,?3)) ",
            "AND (?4<>1 OR NOT EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id) ",
            "OR e.to_whom IN (?1,?2,?3) ",
            "OR EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id AND scope.repo_key IN(SELECT value FROM json_each(?5))) ",
            "OR (e.kind='feedback' AND EXISTS(SELECT 1 FROM actors author WHERE author.id=e.actor_id ",
            "AND author.user=?1 AND author.harness=?2))) AND {strict_scope}"
        ),
        OPEN_QUESTION = OPEN_QUESTION,
        strict_scope = strict_scope
    )
}

pub(in crate::board::local_board) fn open_entries(
    conn: &Connection,
    ctx: &WriteContext,
    scope: &ReadScope,
    limit: usize,
    through: EventSeq,
) -> Result<(Vec<EntryRecord>, usize, bool), BoardError> {
    let predicate = reminder_predicate();
    let identity = ctx.actor.identity();
    let harness = ctx.actor.harness.as_str();
    let scope_sql = ScopeSql::new(scope)?;
    let through = sql_number(through.get());
    let parameters = params![
        ctx.actor.user,
        harness,
        identity,
        scope_sql.kind,
        scope_sql.keys_json,
        through
    ];
    let total: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM (SELECT 1 FROM entries e WHERE {predicate} LIMIT ?7)"),
            params![
                ctx.actor.user,
                harness,
                identity,
                scope_sql.kind,
                scope_sql.keys_json,
                through,
                sql_number(OPEN_REMINDER_COUNT_CAP)
            ],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let records = entries(
        conn,
        &format!(
            "SELECT e.id FROM entries e WHERE {predicate} ORDER BY e.seq,e.id LIMIT {}",
            limit.min(20)
        ),
        parameters,
    )?;
    let total = usize::try_from(total)
        .map_err(|_| invalid("board_unavailable", "invalid reminder count"))?;
    let omitted = total.saturating_sub(records.len());
    Ok((records, omitted, total == OPEN_REMINDER_COUNT_CAP as usize))
}
