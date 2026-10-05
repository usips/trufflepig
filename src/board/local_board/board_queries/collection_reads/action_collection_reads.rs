//! Plan overview and actor-specific attention within the caller's read transaction.

use rusqlite::{Connection, params};

use super::{COLLECTION_LIMIT, claim_ttl, count, sequence_window, validate_limit};
use crate::board::board_domain::board_collections::{
    AttentionReply, EntryCursor, OverviewReply, PlanOverview,
};
use crate::board::board_ids::{EventSeq, PlanId, RepoKey};
use crate::board::board_protocol::{BoardReply, BoardResult};
use crate::board::board_vocabulary::EntryKind;
use crate::board::local_board::board_queries::{board_reads, collection_nested};
use crate::board::local_board::{BoardError, WriteContext, sql_error, sql_number};

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

/// The shared attention entry predicate over `entries e JOIN actors a`,
/// with parameters ?1 user, ?2 host, ?3 harness, ?4 session, ?5 identity,
/// ?6 all, ?7 repo_key, ?8 after seq, ?9 after entry, ?10 through.
/// Every branch implies kind question, proposal, or feedback (proposal
/// rows exist only on proposal entries), so naming the kinds lets SQLite
/// drive the scan from `entries_kind_state` instead of the sequence window.
/// A reader's own user-and-harness questions stay out of "Needs you".
pub(super) fn attention_predicate() -> String {
    let exact_author = "a.user=?1 AND a.host=?2 AND a.harness=?3 AND a.session=?4";
    let plan_authority = concat!(
        "EXISTS(SELECT 1 FROM plans managed WHERE managed.id=e.plan_id AND managed.owner_user=?1 ",
        "AND (?3='human' OR managed.steward=?3))"
    );
    let current_proposal = concat!(
        "EXISTS(SELECT 1 FROM proposals p JOIN plans head ON head.id=p.plan_id WHERE p.entry_id=e.id ",
        "AND p.state='open' AND p.base_revision=head.head_revision AND head.owner_user=?1 ",
        "AND (?3='human' OR head.steward=?3))"
    );
    let stale_proposal = concat!(
        "EXISTS(SELECT 1 FROM proposals p JOIN plans head ON head.id=p.plan_id WHERE p.entry_id=e.id ",
        "AND p.state='open' AND p.base_revision<>head.head_revision)"
    );
    let open_feedback = "e.kind='feedback' AND e.state IN ('open','triaged')";
    let open_question = open_question("?10");
    format!(
        concat!(
            "e.kind IN ('question','proposal','feedback') AND ((({exact_author}) ",
            "AND (({open_feedback}) OR {stale_proposal})) ",
            "OR (({current_proposal} OR (({open_feedback}) AND ({plan_authority}))) ",
            "AND (e.to_whom IS NULL OR e.to_whom IN (?1,?3,?5))) ",
            "OR ((({open_question} AND NOT (a.user=?1 AND a.harness=?3)) OR (({open_feedback}) AND (e.plan_id IS NULL AND a.user=?1 AND ?3='human'))) ",
            "AND (e.to_whom IS NULL OR e.to_whom IN (?1,?3,?5)) ",
            "AND (?6 OR NOT EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id) ",
            "OR e.to_whom IN (?1,?3,?5) OR e.repo_key=?7 ",
            "OR EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id AND scope.repo_key=?7)))) ",
            "AND (e.seq>?8 OR (e.seq=?8 AND e.id>?9)) AND e.seq<=?10"
        ),
        exact_author = exact_author,
        open_feedback = open_feedback,
        stale_proposal = stale_proposal,
        current_proposal = current_proposal,
        plan_authority = plan_authority,
        open_question = open_question
    )
}

pub(in crate::board::local_board) fn overview(
    conn: &Connection,
    ctx: &WriteContext,
    repo_key: Option<&RepoKey>,
    after: Option<PlanId>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    validate_limit(limit, COLLECTION_LIMIT)?;
    let (_, through) = sequence_window(conn, None, through)?;
    let predicate = concat!(
        "(?1 IS NULL OR EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=p.id AND scope.repo_key=?1) ",
        "OR NOT EXISTS(SELECT 1 FROM plan_repos s WHERE s.plan_id=p.id)) ",
        "AND p.id>?2 AND EXISTS(SELECT 1 FROM revisions initial ",
        "WHERE initial.plan_id=p.id AND initial.number=1 AND initial.seq<=?3)"
    );
    let mut statement = conn
        .prepare(&format!(
            concat!(
                "SELECT p.id,p.title,p.owner_user,p.steward,p.head_revision,p.created_at FROM plans p ",
                "WHERE {predicate} ORDER BY p.id LIMIT ?4"
            ),
            predicate = predicate
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            repo_key.map(RepoKey::as_str),
            sql_number(after.map_or(0, PlanId::get)),
            sql_number(through.get()),
            sql_number((limit + 1) as u64)
        ])
        .map_err(sql_error)?;
    let mut records = Vec::with_capacity(limit + 1);
    while let Some(row) = rows.next().map_err(sql_error)? {
        records.push(board_reads::plan_row(row)?);
    }
    let next_after = if records.len() > limit {
        records.truncate(limit);
        records.last().map(|plan| plan.id)
    } else {
        None
    };
    let total = count(
        conn,
        &format!("SELECT count(*) FROM plans p WHERE {predicate}"),
        params![
            repo_key.map(RepoKey::as_str),
            sql_number(after.map_or(0, PlanId::get)),
            sql_number(through.get())
        ],
    )?;
    let mut plans = Vec::with_capacity(records.len());
    let open_question = open_question("?2");
    for plan in records {
        let tasks =
            collection_nested::task_window(conn, plan.id, None, None, Some(through), NESTED_LIMIT)?;
        let tasks_omitted = tasks.omitted;
        let task_ceiling = tasks.ceiling;
        let tasks = tasks.tasks;
        let claims = collection_nested::claim_window(
            conn,
            ctx,
            Some(plan.id),
            false,
            None,
            true,
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
            plan,
            tasks,
            task_ceiling,
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
            repo_key: repo_key.cloned(),
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
    repo_key: Option<&RepoKey>,
    all: bool,
    after: Option<EntryCursor>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    validate_limit(limit, COLLECTION_LIMIT)?;
    let (_, through) = sequence_window(conn, after.map(|cursor| cursor.seq), through)?;
    let predicate = attention_predicate();
    let identity = ctx.actor.identity();
    let parameters = params![
        ctx.actor.user,
        ctx.actor.host,
        ctx.actor.harness.as_str(),
        ctx.actor.session,
        identity,
        all,
        repo_key.map(RepoKey::as_str),
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
            all,
            repo_key.map(RepoKey::as_str),
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
    let claims = collection_nested::claim_window(
        conn,
        ctx,
        None,
        true,
        repo_key,
        all,
        None,
        Some(through),
        limit,
    )?;
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
                "SELECT EXISTS(SELECT 1 FROM proposals p JOIN plans head ON head.id=p.plan_id WHERE p.entry_id=?1 ",
                "AND p.state='open' AND p.base_revision<>head.head_revision)"
            ),
            [sql_number(entry.id.get())], |row| row.get(0),
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
            repo_key: repo_key.cloned(),
            all,
            server_now: ctx.now,
            claim_ttl_secs: claim_ttl(ctx),
            after,
            through,
            next_after,
            claims_next_after,
        }),
    ))
}
