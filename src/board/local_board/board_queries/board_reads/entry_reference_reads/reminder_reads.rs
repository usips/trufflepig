//! Open-reminder predicate and capped reminder totals.
use super::*;

/// Reminder totals are exact up to this cap; larger backlogs report a lower bound.
const OPEN_REMINDER_COUNT_CAP: u64 = 200;

/// Open-reminder predicate over `entries e`: ?1 user, ?2 harness, ?3 identity,
/// ?4 all, ?5 repo_key, ?6 through. The kind prefix drives the scan from
/// `entries_kind_state`; authorship matches user and harness so a new session
/// of the same harness keeps its reminders; proposal currency is as of ?6.
pub(in crate::board::local_board::board_queries::board_reads) fn reminder_predicate() -> String {
    format!(
        concat!(
            "e.kind IN ('question','proposal','feedback') ",
            "AND ((EXISTS(SELECT 1 FROM proposals p WHERE p.entry_id=e.id ",
            "AND p.state='open' AND (p.base_revision=(SELECT max(number) FROM revisions head_at ",
            "WHERE head_at.plan_id=p.plan_id AND head_at.seq<=?6) OR EXISTS(SELECT 1 FROM actors author ",
            "WHERE author.id=e.actor_id AND author.user=?1 AND author.harness=?2))) ",
            "OR (e.kind='feedback' AND e.state IN ('open','triaged')) OR ({OPEN_QUESTION}))) ",
            "AND (e.to_whom IS NULL OR e.to_whom IN (?1,?2,?3)) ",
            "AND (?4 OR NOT EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id) ",
            "OR e.to_whom IN (?1,?2,?3) ",
            "OR EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id AND scope.repo_key=?5) ",
            "OR (e.kind='feedback' AND EXISTS(SELECT 1 FROM actors author WHERE author.id=e.actor_id ",
            "AND author.user=?1 AND author.harness=?2)))"
        ),
        OPEN_QUESTION = OPEN_QUESTION
    )
}

pub(in crate::board::local_board) fn open_entries(
    conn: &Connection,
    ctx: &WriteContext,
    repo_key: Option<&RepoKey>,
    all: bool,
    limit: usize,
    through: EventSeq,
) -> Result<(Vec<EntryRecord>, usize, bool), BoardError> {
    let predicate = reminder_predicate();
    let identity = ctx.actor.identity();
    let harness = ctx.actor.harness.as_str();
    let repo = repo_key.map(RepoKey::as_str);
    let through = sql_number(through.get());
    let parameters = params![ctx.actor.user, harness, identity, all, repo, through];
    let total: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM (SELECT 1 FROM entries e WHERE {predicate} LIMIT ?7)"),
            params![
                ctx.actor.user,
                harness,
                identity,
                all,
                repo,
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
