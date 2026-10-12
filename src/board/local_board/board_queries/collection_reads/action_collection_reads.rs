//! Plan overview and actor-specific attention within the caller's read transaction.
mod attention_predicates;

pub(super) use attention_predicates::attention_predicate;
use rusqlite::{Connection, params};

use super::{ACTIVE_TASKS, COLLECTION_LIMIT, claim_ttl, count, sequence_window, validate_limit};
use crate::board::board_domain::board_collections::{
    AttentionReply, EntryCursor, OverviewCursor, OverviewReply, PlanOverview,
};
use crate::board::board_ids::EventSeq;
use crate::board::board_protocol::{BoardReply, BoardResult, ReadScope};
use crate::board::board_vocabulary::EntryKind;
use crate::board::local_board::board_queries::read_scope_sql::{ScopeSql, plan_repo_keys};
use crate::board::local_board::board_queries::{board_reads, collection_nested};
use crate::board::local_board::{
    BoardError, WriteContext, invalid, row_number, sql_error, sql_number,
};

const NESTED_LIMIT: usize = 20;

/// Open-question branches with answers and corrections visible at `through`.
fn open_question(through: &str) -> String {
    format!(
        concat!(
            "e.kind='question' AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs reference ",
            "ON reference.entry_id=answer.id WHERE answer.kind='answer' AND reference.target='E'||e.id ",
            "AND answer.seq<={through}) ",
            "AND NOT EXISTS(SELECT 1 FROM entries correction WHERE correction.supersedes=e.id ",
            "AND correction.seq<={through})"
        ),
        through = through
    )
}

pub(in crate::board::local_board) fn overview(
    conn: &Connection,
    ctx: &WriteContext,
    scope: &ReadScope,
    after: Option<OverviewCursor>,
    active_only: bool,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    validate_limit(limit, COLLECTION_LIMIT)?;
    let (_, through) = sequence_window(conn, None, through)?;
    if let Some(after) = after {
        after.validate().map_err(BoardError::from)?;
        if after.seq > through {
            return Err(invalid(
                "invalid_reference",
                "overview cursor exceeds through",
            ));
        }
    }
    let scope_sql = ScopeSql::new(scope)?;
    let membership = ScopeSql::plan_predicate("p.id", 1, 2);
    let active_filter = if active_only {
        format!("AND EXISTS(SELECT 1 FROM tasks t WHERE t.plan_id=p.id AND {ACTIVE_TASKS})")
    } else {
        String::new()
    };
    let candidates = format!(
        concat!(
            "WITH candidates AS (SELECT p.id,p.title,p.owner_user,p.steward,p.head_revision,p.created_at,",
            "coalesce((SELECT max(e.seq) FROM events e WHERE e.plan_id=p.id AND e.seq<=?5),",
            "(SELECT initial.seq FROM revisions initial WHERE initial.plan_id=p.id AND initial.number=1 ",
            "AND initial.seq<=?5)) AS activity_seq FROM plans p WHERE {membership} ",
            "AND EXISTS(SELECT 1 FROM revisions initial WHERE initial.plan_id=p.id ",
            "AND initial.number=1 AND initial.seq<=?5) {active_filter}) "
        ),
        membership = membership,
        active_filter = active_filter,
    );
    let keyset = "(?3=0 OR activity_seq<?3 OR (activity_seq=?3 AND id<?4))";
    let mut statement = conn
        .prepare(&format!(
            concat!(
                "{candidates} SELECT id,title,owner_user,steward,head_revision,created_at,activity_seq ",
                "FROM candidates WHERE {keyset} ORDER BY activity_seq DESC,id DESC LIMIT ?6"
            ),
            candidates = candidates,
            keyset = keyset,
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            scope_sql.kind,
            scope_sql.keys_json,
            sql_number(after.map_or(0, |cursor| cursor.seq.get())),
            sql_number(after.map_or(0, |cursor| cursor.plan.get())),
            sql_number(through.get()),
            sql_number((limit + 1) as u64)
        ])
        .map_err(sql_error)?;
    let mut records = Vec::with_capacity(limit + 1);
    while let Some(row) = rows.next().map_err(sql_error)? {
        records.push((
            board_reads::plan_row(row)?,
            EventSeq::new(row_number(row, 6).map_err(sql_error)?),
        ));
    }
    let next_after = if records.len() > limit {
        records.truncate(limit);
        records.last().map(|(plan, seq)| OverviewCursor {
            seq: *seq,
            plan: plan.id,
        })
    } else {
        None
    };
    let total = count(
        conn,
        &format!("{candidates} SELECT count(*) FROM candidates WHERE {keyset}"),
        params![
            scope_sql.kind,
            scope_sql.keys_json,
            sql_number(after.map_or(0, |cursor| cursor.seq.get())),
            sql_number(after.map_or(0, |cursor| cursor.plan.get())),
            sql_number(through.get())
        ],
    )?;
    let mut plans = Vec::with_capacity(records.len());
    let open_question = open_question("?2");
    for (plan, updated_seq) in records {
        let tasks = collection_nested::overview_task_window(conn, plan.id, NESTED_LIMIT)?;
        let tasks_omitted = tasks.omitted;
        let task_ceiling = tasks.ceiling;
        let done_count = tasks.done_count;
        let recent_done = tasks.recent_done;
        let tasks = tasks.tasks;
        let claims = collection_nested::claim_window(
            conn,
            ctx,
            Some(plan.id),
            false,
            &ReadScope::All,
            None,
            Some(through),
            NESTED_LIMIT,
        )?;
        let claims_omitted = claims.omitted;
        let claims = claims.claims.into_iter().map(|view| view.claim).collect();
        let open_questions = count(
            conn,
            &format!(
                "SELECT count(*) FROM entries e WHERE e.plan_id=?1 AND e.seq<=?2 AND ({open_question})",
                open_question = open_question
            ),
            params![sql_number(plan.id.get()), sql_number(through.get())],
        )?;
        let open_proposals = count(
            conn,
            concat!(
                "SELECT count(*) FROM proposals p JOIN entries e ON e.id=p.entry_id WHERE p.plan_id=?1 ",
                "AND p.state='open' AND e.seq<=?2"
            ),
            params![sql_number(plan.id.get()), sql_number(through.get())],
        )?;
        let open_feedback = count(
            conn,
            concat!(
                "SELECT count(*) FROM entries WHERE plan_id=?1 AND kind='feedback' AND state IN ('open','triaged') ",
                "AND seq<=?2"
            ),
            params![sql_number(plan.id.get()), sql_number(through.get())],
        )?;
        plans.push(PlanOverview {
            repo_keys: plan_repo_keys(conn, plan.id)?,
            plan,
            updated_seq,
            tasks,
            task_ceiling,
            done_count,
            recent_done,
            claims,
            open_questions,
            open_proposals,
            open_feedback,
            tasks_omitted,
            claims_omitted,
        });
    }
    Ok(BoardReply::new(
        "local",
        BoardResult::Overview(OverviewReply {
            omitted: total.saturating_sub(plans.len()),
            plans,
            scope: scope.clone(),
            server_now: ctx.now,
            claim_ttl_secs: claim_ttl(ctx),
            after,
            through,
            next_after,
        }),
    ))
}

pub(in crate::board::local_board) fn attention(
    conn: &Connection,
    ctx: &WriteContext,
    scope: &ReadScope,
    after: Option<EntryCursor>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    validate_limit(limit, COLLECTION_LIMIT)?;
    let (_, through) = sequence_window(conn, after.map(|cursor| cursor.seq), through)?;
    let predicate = attention_predicate();
    let scope_sql = ScopeSql::new(scope)?;
    let identity = ctx.actor.identity();
    let parameters = params![
        ctx.actor.user,
        ctx.actor.host,
        ctx.actor.harness.as_str(),
        ctx.actor.session,
        identity,
        scope_sql.kind,
        scope_sql.keys_json,
        sql_number(after.map_or(0, |cursor| cursor.seq.get())),
        sql_number(after.map_or(0, |cursor| cursor.entry.get())),
        sql_number(through.get())
    ];
    let mut entries = board_reads::entries(
        conn,
        &format!(
            concat!(
                "SELECT e.id FROM entries e JOIN actors a ON a.id=e.actor_id WHERE {predicate} ",
                "ORDER BY e.seq,e.id LIMIT ?11"
            ),
            predicate = predicate
        ),
        params![
            ctx.actor.user,
            ctx.actor.host,
            ctx.actor.harness.as_str(),
            ctx.actor.session,
            identity,
            scope_sql.kind,
            scope_sql.keys_json,
            sql_number(after.map_or(0, |cursor| cursor.seq.get())),
            sql_number(after.map_or(0, |cursor| cursor.entry.get())),
            sql_number(through.get()),
            sql_number((limit + 1) as u64)
        ],
    )?;
    // The exact omitted count is paid for only when the page overflows;
    // a short page already proves nothing was omitted.
    let total = if entries.len() > limit {
        count(
            conn,
            &format!(
                "SELECT count(*) FROM entries e JOIN actors a ON a.id=e.actor_id WHERE {predicate}"
            ),
            parameters,
        )?
    } else {
        entries.len()
    };
    let next_after = if entries.len() > limit {
        entries.truncate(limit);
        entries.last().map(|entry| EntryCursor {
            seq: entry.seq,
            entry: entry.id,
        })
    } else {
        None
    };
    let claims =
        collection_nested::claim_window(conn, ctx, None, true, scope, None, Some(through), limit)?;
    let claims_omitted = claims.omitted;
    let claims_next_after = claims.next_after;
    let stale_claims = claims.claims;
    let mut rebase_needed = Vec::with_capacity(entries.len());
    for entry in &entries {
        if entry.kind != EntryKind::Proposal || entry.actor != ctx.actor {
            continue;
        }
        let stale: bool = conn.query_row(
            concat!(
                "SELECT EXISTS(SELECT 1 FROM proposals p WHERE p.entry_id=?1 AND p.state='open' ",
                "AND p.base_revision<>(SELECT max(number) FROM revisions head_at WHERE head_at.plan_id=p.plan_id ",
                "AND head_at.seq<=?2))"
            ),
            params![sql_number(entry.id.get()), sql_number(through.get())],
            |row| row.get(0),
        ).map_err(sql_error)?;
        if stale {
            rebase_needed.push(entry.id);
        }
    }
    Ok(BoardReply::new(
        "local",
        BoardResult::Attention(AttentionReply {
            actor: ctx.actor.clone(),
            entries_omitted: total.saturating_sub(entries.len()),
            entries,
            stale_claims,
            claims_omitted,
            rebase_needed,
            scope: scope.clone(),
            server_now: ctx.now,
            claim_ttl_secs: claim_ttl(ctx),
            after,
            through,
            next_after,
            claims_next_after,
        }),
    ))
}
