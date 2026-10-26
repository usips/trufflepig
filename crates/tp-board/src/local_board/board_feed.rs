//! Fresh events and separate reminders; only explicit acknowledgements move cursors.

#[cfg(test)]
mod tests;

use crate::board_protocol::BoardErrorCode;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::hash::{DefaultHasher, Hash, Hasher};

use super::board_queries::board_reads;
use super::board_queries::board_reads::recipient_predicates::{
    actor_origin_from_row, addressed_to_actor_sql, authority_from_row, event_task_owner_human_sql,
    event_task_question_sql, worker_event_to_actor_sql,
};
use super::board_queries::read_scope_sql::ScopeSql;
use super::board_writes::task_claims;
use super::{
    BoardError, WriteContext, actor_from_row, invalid, max_seq, row_number, sql_error, sql_number,
    sqlite_u64,
};
use crate::board_actor::{BoardRecipient, claim_vendor};
use crate::board_ids::{BoardRef, EntryId, EventSeq, PlanId};
use crate::board_protocol::{
    AuthorityRole, BoardReply, BoardResult, EventRecord, FeedbackVia, InboxReply, InboxWait,
    PendingNotice, PendingNoticeKind, PendingNoticeReason, PendingReply, ReadScope, WorkerAddress,
};
use crate::board_vocabulary::{EntryKind, EntryText};

const FIRST_FEED_LIMIT: usize = 20;
/// A first inbox examines at most this many of the newest events for its seed.
const FIRST_FEED_SCAN: u64 = 500;
const EVENT_SELECT: &str = concat!(
    "SELECT e.seq,e.plan_id,e.kind,e.subject,e.to_whom,a.user,a.host,a.harness,a.session,e.model,",
    "e.effort,e.summary,e.created_at,(SELECT evidence.via FROM entries evidence ",
    "WHERE evidence.seq=e.seq AND evidence.actor_id=e.actor_id ORDER BY evidence.id LIMIT 1) ",
    ",e.origin_harness,e.origin_session,e.origin_ip,e.authority_basis,e.authority_entry ",
    "FROM events e JOIN actors a ON a.id=e.actor_id"
);

fn relevant_event_predicate() -> String {
    let addressed = addressed_to_actor_sql("e.to_whom", "?3", "?4", "?5");
    let task_question = event_task_question_sql("e");
    let task_owner = event_task_owner_human_sql("e", "?3", "?4");
    let public_event =
        format!("(e.to_whom IS NULL AND NOT ({task_question})) OR {addressed} OR {task_owner}");
    format!(
        concat!(
            "e.kind<>'hello' AND e.actor_id<>?1 ",
            "AND (((e.kind IN ('claim','task') OR ({public_event})) ",
            "AND (?7<>1 OR (e.plan_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id)) ",
            "OR {addressed} ",
            "OR EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id AND scope.repo_key IN(SELECT value FROM json_each(?8))) ",
            "OR EXISTS(SELECT 1 FROM entries evidence JOIN plan_repos scope ON scope.plan_id=evidence.plan_id ",
            "WHERE evidence.seq=e.seq AND scope.repo_key IN(SELECT value FROM json_each(?8))))) ",
            "OR (e.kind='feedback' AND EXISTS(SELECT 1 FROM entries report JOIN actors author ON author.id=report.actor_id ",
            "WHERE report.kind='feedback' AND author.user=?3 AND author.harness=?4 AND 'E'||report.id=e.subject ",
            "AND (e.to_whom IS NULL OR {addressed} ",
            "OR e.to_whom=author.user||'@'||author.host||'/'||author.harness||'/'||author.session))))"
        ),
        public_event = public_event,
        addressed = addressed,
    )
}

pub(super) fn inbox(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    after: Option<EventSeq>,
    limit: usize,
    scope: &ReadScope,
) -> Result<BoardReply, BoardError> {
    if !(1..=2000).contains(&limit) {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "inbox limit must be 1..2000",
        ));
    }
    if after.is_none() {
        task_claims::refresh_inbox_claims(tx, ctx.actor_id, ctx.now)?;
    }
    let stored: Option<i64> = tx
        .query_row(
            "SELECT cursor_seq FROM agent_sessions WHERE actor_id=?1",
            [ctx.actor_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .flatten();
    let cursor = EventSeq::new(stored.map(sqlite_u64).transpose()?.unwrap_or(0));
    let first = after.is_none() && stored.is_none();
    let latest = max_seq(tx)?;
    let start = if first {
        EventSeq::new(latest.get().saturating_sub(FIRST_FEED_SCAN))
    } else {
        after.unwrap_or(cursor)
    };
    let count = if first {
        limit.min(FIRST_FEED_LIMIT)
    } else {
        limit
    };
    let order = if first { "DESC" } else { "ASC" };
    let scope_sql = ScopeSql::new(scope)?;
    let membership = ScopeSql::event_predicate(7, 8);
    let relevant_event = relevant_event_predicate();
    let sql = format!(
        "{EVENT_SELECT} WHERE ({relevant_event}) AND (?7<2 OR {membership}) \
         AND e.seq>?2 ORDER BY e.seq {order} LIMIT ?6"
    );
    let mut statement = tx.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            ctx.actor_id,
            sql_number(start.get()),
            ctx.actor.user,
            ctx.actor.harness.as_str(),
            ctx.actor.identity(),
            sql_number(count as u64 + 1),
            scope_sql.kind,
            scope_sql.keys_json,
        ])
        .map_err(sql_error)?;
    let mut events = Vec::with_capacity(count + 1);
    while let Some(row) = rows.next().map_err(sql_error)? {
        events.push(event_row(row)?);
    }
    let next = if events.len() > count {
        events.pop()
    } else {
        None
    };
    if first {
        events.reverse();
    }
    let query_truncated = !first && next.is_some();
    let scanned_through = if first {
        latest
    } else {
        next.map_or(latest, |event| EventSeq::new(event.seq.get() - 1))
    };
    let (open, open_omitted, open_omitted_lower_bound) =
        board_reads::open_entries(tx, ctx, scope, limit.min(20), latest)?;
    Ok(BoardReply::new(
        "local",
        BoardResult::Inbox(InboxReply {
            actor: ctx.actor.clone(),
            cursor,
            scanned_through,
            query_truncated,
            events,
            open,
            open_omitted,
            open_omitted_lower_bound,
            scope: scope.clone(),
            latest,
            advancing: after.is_none(),
            wait: InboxWait::None,
        }),
    ))
}

/// Reads addressed work and open reminders without changing session or claim state.
pub(super) fn pending(
    conn: &Connection,
    ctx: &WriteContext,
    after: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    if !(1..=200).contains(&limit) {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "pending limit must be 1..200",
        ));
    }
    let actor_known: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM actors WHERE id=?1)",
            [ctx.actor_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if !actor_known {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            "pending requires a known board actor",
        ));
    }

    let through = max_seq(conn)?;
    if after.is_some_and(|cursor| cursor > through) {
        return Err(invalid(
            BoardErrorCode::InvalidReference,
            "pending cursor exceeds through",
        ));
    }
    let generation = pending_generation(conn, ctx.actor_id)?;
    let candidates = pending_candidates();
    let identity = ctx.actor.identity();
    let total: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM ({candidates})"),
            params![
                ctx.actor_id,
                identity,
                ctx.actor.user,
                ctx.actor.host,
                ctx.actor.harness.as_str(),
                ctx.actor.session,
                ctx.now,
                sql_number(after.map_or(0, EventSeq::get)),
                sql_number(through.get()),
            ],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let mut statement = conn
        .prepare(&format!(
            "SELECT seq,payload_kind,payload_id,reason FROM ({candidates}) \
             ORDER BY CASE WHEN seq>?8 THEN 0 ELSE 1 END,seq,payload_kind LIMIT ?10"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            ctx.actor_id,
            identity,
            ctx.actor.user,
            ctx.actor.host,
            ctx.actor.harness.as_str(),
            ctx.actor.session,
            ctx.now,
            sql_number(after.map_or(0, EventSeq::get)),
            sql_number(through.get()),
            (limit + 1) as i64,
        ])
        .map_err(sql_error)?;
    let mut descriptors = Vec::with_capacity(limit + 1);
    while let Some(row) = rows.next().map_err(sql_error)? {
        let seq = EventSeq::new(row_number(row, 0).map_err(sql_error)?);
        let kind: String = row.get(1).map_err(sql_error)?;
        let id: Option<i64> = row.get(2).map_err(sql_error)?;
        let reason: String = row.get(3).map_err(sql_error)?;
        descriptors.push((seq, kind, id, reason));
    }
    descriptors.truncate(limit);

    let mut notices = Vec::with_capacity(descriptors.len());
    for (seq, kind, id, reason) in descriptors {
        let reason = parse_pending_reason(&reason)?;
        let notice = if kind == "entry" {
            let id = id.ok_or_else(|| {
                invalid(BoardErrorCode::BoardUnavailable, "pending entry has no id")
            })?;
            let id = EntryId::new(sqlite_u64(id)?).map_err(BoardError::from)?;
            PendingNotice {
                seq,
                kind: PendingNoticeKind::Entry,
                reason,
                entry: Some(board_reads::entry(conn, id)?),
                event: None,
            }
        } else if kind == "event" {
            let mut statement = conn
                .prepare(&format!("{EVENT_SELECT} WHERE e.seq=?1"))
                .map_err(sql_error)?;
            let mut rows = statement
                .query([sql_number(seq.get())])
                .map_err(sql_error)?;
            let row = rows.next().map_err(sql_error)?.ok_or_else(|| {
                invalid(
                    BoardErrorCode::BoardUnavailable,
                    "pending event disappeared",
                )
            })?;
            let event = event_row(row)?;
            PendingNotice {
                seq,
                kind: PendingNoticeKind::Event,
                reason,
                entry: None,
                event: Some(event),
            }
        } else {
            return Err(invalid(
                BoardErrorCode::BoardUnavailable,
                "unknown pending payload kind",
            ));
        };
        notice.validate().map_err(BoardError::from)?;
        notices.push(notice);
    }

    let omitted = usize::try_from(total)
        .map_err(|_| {
            invalid(
                BoardErrorCode::BoardUnavailable,
                "pending count exceeds platform range",
            )
        })?
        .saturating_sub(notices.len());
    Ok(BoardReply::new(
        "local",
        BoardResult::Pending(PendingReply {
            notices,
            after,
            through,
            server_now: ctx.now,
            generation,
            omitted,
        }),
    ))
}

fn pending_candidates() -> String {
    let identity = "?2";
    let user = "?3";
    let host = "?4";
    let harness = "?5";
    let session = "?6";
    let addressed = addressed_to_actor_sql("e.to_whom", user, harness, identity);
    let task_ref_addressed = addressed_to_actor_sql("task_ref.target", user, harness, identity);
    let task_owner =
        super::board_queries::board_reads::recipient_predicates::task_owner_human_question_sql(
            "e.id", user, harness,
        );
    let task_ref = super::board_queries::board_reads::recipient_predicates::task_entry_sql("e.id");
    let question_recipient = format!(
        "({addressed} OR {task_owner} OR (e.to_whom IS NULL AND NOT ({task_ref})) OR \
         (e.to_whom IS NULL AND EXISTS(SELECT 1 FROM entry_refs task_ref \
         WHERE task_ref.entry_id=e.id AND {task_ref_addressed})))"
    );
    let open_question = open_question_at("?9");
    let decide_authority = crate::local_board::board_authority::authority_predicate_sql(
        AuthorityRole::Decide,
        "p.plan_id",
        "head.owner_user",
        user,
        host,
        harness,
        session,
        identity,
        "?7",
    );
    let triage_authority = crate::local_board::board_authority::authority_predicate_sql(
        AuthorityRole::Triage,
        "e.plan_id",
        "coalesce((SELECT managed.owner_user FROM plans managed WHERE managed.id=e.plan_id),author.user)",
        user,
        host,
        harness,
        session,
        identity,
        "?7",
    );
    let proposal = format!(
        concat!(
            "e.kind='proposal' AND EXISTS(SELECT 1 FROM proposals p JOIN plans head ON head.id=p.plan_id ",
            "WHERE p.entry_id=e.id AND p.state='open' AND p.base_revision=(",
            "SELECT max(number) FROM revisions head_at WHERE head_at.plan_id=p.plan_id AND head_at.seq<=?9) ",
            "AND ((e.to_whom IS NULL AND {decide_authority}) OR {addressed}))"
        ),
        decide_authority = decide_authority,
        addressed = addressed,
    );
    let feedback = format!(
        "e.kind='feedback' AND e.state IN ('open','triaged') AND ((e.to_whom IS NULL AND {triage_authority}) OR {addressed})",
        triage_authority = triage_authority,
        addressed = addressed,
    );
    let worker_end = format!("e.kind='worker_end' AND {addressed}", addressed = addressed);
    let question = format!(
        "e.kind='question' AND ({open_question}) AND {question_recipient}",
        open_question = open_question,
        question_recipient = question_recipient,
    );
    let worker_event = worker_event_to_actor_sql("e", user, harness, identity);
    let event_task_q = event_task_question_sql("e");
    let event_task_owner = event_task_owner_human_sql("e", user, harness);
    let event_task_recipient = format!(
        concat!(
            "({event_task_owner} OR EXISTS(SELECT 1 FROM entries task_question ",
            "JOIN entry_refs task_ref ON task_ref.entry_id=task_question.id ",
            "WHERE e.subject='E'||task_question.id AND task_question.kind='question' ",
            "AND e.to_whom IS NULL AND task_question.to_whom IS NULL ",
            "AND {task_ref_addressed}))"
        ),
        event_task_owner = event_task_owner,
        task_ref_addressed = task_ref_addressed,
    );
    let event_addressed = addressed_to_actor_sql("e.to_whom", user, harness, identity);
    format!(
        concat!(
            "SELECT e.seq AS seq,'entry' AS payload_kind,e.id AS payload_id,",
            "CASE WHEN e.kind='question' THEN 'question' WHEN e.kind='worker_end' THEN 'worker' ",
            "ELSE 'mention' END AS reason ",
            "FROM entries e JOIN actors author ON author.id=e.actor_id ",
            "WHERE e.actor_id<>?1 AND e.seq<=?9 AND ({question} OR {proposal} OR {feedback} OR ({worker_end})) ",
            "AND (e.seq>?8 OR e.kind='question') ",
            "UNION ALL ",
            "SELECT e.seq AS seq,'event' AS payload_kind,NULL AS payload_id,",
            "CASE WHEN e.kind='claim' THEN 'claim' WHEN e.kind IN ('worker_add','worker_end') THEN 'worker' ELSE 'mention' END AS reason ",
            "FROM events e JOIN actors author ON author.id=e.actor_id ",
            "WHERE e.actor_id<>?1 AND e.seq<=?9 AND ",
            "({event_addressed} OR {worker_event} OR ({event_task_q} AND {event_task_recipient})) ",
            "AND e.seq>?8 AND NOT EXISTS(SELECT 1 FROM entries post ",
            "WHERE post.id=CAST(substr(e.subject,2) AS INTEGER) ",
            "AND e.subject='E'||post.id AND post.kind IN ('question','proposal','feedback'))"
        ),
        question = question,
        proposal = proposal,
        feedback = feedback,
        worker_end = worker_end,
        event_addressed = event_addressed,
        worker_event = worker_event,
        event_task_q = event_task_q,
        event_task_recipient = event_task_recipient,
    )
}

fn open_question_at(through: &str) -> String {
    format!(
        concat!(
            "e.kind='question' AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs reference ",
            "ON reference.entry_id=answer.id WHERE answer.kind='answer' AND reference.target='E'||e.id ",
            "AND answer.seq<={through}) AND NOT EXISTS(SELECT 1 FROM entries correction ",
            "WHERE correction.supersedes=e.id AND correction.seq<={through})"
        ),
        through = through,
    )
}

fn parse_pending_reason(value: &str) -> Result<PendingNoticeReason, BoardError> {
    match value {
        "mention" => Ok(PendingNoticeReason::Mention),
        "question" => Ok(PendingNoticeReason::Question),
        "claim" => Ok(PendingNoticeReason::Claim),
        "worker" => Ok(PendingNoticeReason::Worker),
        _ => Err(invalid(
            BoardErrorCode::BoardUnavailable,
            "unknown pending notice reason",
        )),
    }
}

fn pending_generation(conn: &Connection, actor_id: i64) -> Result<String, BoardError> {
    let sql = concat!(
        "WITH actor_tasks(plan_id,task_ordinal) AS (",
        "SELECT plan_id,task_ordinal FROM claims WHERE actor_id=?1 AND ended_at IS NULL ",
        "UNION SELECT w.plan_id,w.task_ordinal FROM claim_workers w JOIN claims c ON c.id=w.claim_id ",
        "WHERE w.actor_id=?1 AND w.ended_at IS NULL AND c.ended_at IS NULL), ",
        "generation_rows(kind,plan_id,task_ordinal,entry_id,row_id,handle,actor_id) AS (",
        "SELECT 'claim',c.plan_id,c.task_ordinal,c.entry_id,c.id,'',c.actor_id ",
        "FROM claims c JOIN actor_tasks t ON t.plan_id=c.plan_id AND t.task_ordinal=c.task_ordinal ",
        "WHERE c.ended_at IS NULL UNION ALL ",
        "SELECT 'worker',w.plan_id,w.task_ordinal,c.entry_id,w.id,w.handle,w.actor_id ",
        "FROM claim_workers w JOIN claims c ON c.id=w.claim_id ",
        "JOIN actor_tasks t ON t.plan_id=w.plan_id AND t.task_ordinal=w.task_ordinal ",
        "WHERE w.ended_at IS NULL AND c.ended_at IS NULL) ",
        "SELECT kind,plan_id,task_ordinal,entry_id,row_id,handle,actor_id ",
        "FROM generation_rows ORDER BY plan_id,task_ordinal,kind,row_id"
    );
    let mut statement = conn.prepare(sql).map_err(sql_error)?;
    let mut rows = statement.query([actor_id]).map_err(sql_error)?;
    let mut hasher = DefaultHasher::new();
    actor_id.hash(&mut hasher);
    while let Some(row) = rows.next().map_err(sql_error)? {
        let kind: String = row.get(0).map_err(sql_error)?;
        let plan: i64 = row.get(1).map_err(sql_error)?;
        let ordinal: i64 = row.get(2).map_err(sql_error)?;
        let entry: i64 = row.get(3).map_err(sql_error)?;
        let row_id: i64 = row.get(4).map_err(sql_error)?;
        let handle: String = row.get(5).map_err(sql_error)?;
        let binding_actor: Option<i64> = row.get(6).map_err(sql_error)?;
        kind.hash(&mut hasher);
        plan.hash(&mut hasher);
        ordinal.hash(&mut hasher);
        entry.hash(&mut hasher);
        row_id.hash(&mut hasher);
        handle.hash(&mut hasher);
        binding_actor.hash(&mut hasher);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn event_row(row: &rusqlite::Row<'_>) -> Result<EventRecord, BoardError> {
    let to: Option<String> = row.get(4).map_err(sql_error)?;
    let plan: Option<i64> = row.get(1).map_err(sql_error)?;
    let actor = actor_from_row(row, 5).map_err(sql_error)?;
    let model: Option<String> = row.get(9).map_err(sql_error)?;
    let kind: EntryKind = row
        .get::<_, String>(2)
        .map_err(sql_error)?
        .parse()
        .map_err(BoardError::from)?;
    let subject: String = row.get(3).map_err(sql_error)?;
    let subject = if matches!(kind, EntryKind::WorkerAdd | EntryKind::WorkerEnd) {
        // EventRecord uses BoardRef; retain the worker event's parent task address.
        let worker = WorkerAddress::parse(&subject).map_err(BoardError::from)?;
        BoardRef::Task(worker.task)
    } else {
        subject.parse::<BoardRef>().map_err(BoardError::from)?
    };
    Ok(EventRecord {
        via: row
            .get::<_, Option<String>>(13)
            .map_err(sql_error)?
            .as_deref()
            .map(FeedbackVia::parse)
            .transpose()
            .map_err(|error| invalid(BoardErrorCode::BoardUnavailable, error.to_string()))?,
        seq: EventSeq::new(row_number(row, 0).map_err(sql_error)?),
        plan: plan
            .map(sqlite_u64)
            .transpose()?
            .map(PlanId::new)
            .transpose()
            .map_err(BoardError::from)?,
        kind,
        subject,
        to: to
            .as_deref()
            .map(BoardRecipient::parse)
            .transpose()
            .map_err(BoardError::from)?,
        vendor: claim_vendor(&actor.harness, model.as_deref()),
        actor,
        model,
        effort: row.get(10).map_err(sql_error)?,
        summary: EntryText::new(row.get::<_, String>(11).map_err(sql_error)?)
            .map_err(BoardError::from)?,
        created_at: row.get(12).map_err(sql_error)?,
        origin: actor_origin_from_row(row, 14, 15, 16)?,
        authority: authority_from_row(row, 17, 18)?,
    })
}

pub(super) fn read_events(
    conn: &Connection,
    after: EventSeq,
    through: EventSeq,
    plan: Option<PlanId>,
    scope: &ReadScope,
    limit: usize,
) -> Result<Vec<EventRecord>, BoardError> {
    if !(1..=501).contains(&limit) {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "event batch limit must be 1..501",
        ));
    }
    let scope_sql = ScopeSql::new(scope)?;
    let membership = ScopeSql::event_predicate(5, 6);
    let requested_plan = ScopeSql::plan_predicate("?3", 5, 6);
    let mut statement = conn
        .prepare(&format!(
            concat!(
                "{EVENT_SELECT} WHERE e.seq>?1 AND e.seq<=?2 ",
                "AND (?3 IS NULL OR e.plan_id=?3 OR EXISTS(SELECT 1 FROM entries evidence ",
                "WHERE evidence.seq=e.seq AND evidence.plan_id=?3)) ",
                "AND (?3 IS NULL OR {requested_plan}) AND {membership} ORDER BY e.seq LIMIT ?4"
            ),
            EVENT_SELECT = EVENT_SELECT,
            membership = membership,
            requested_plan = requested_plan,
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(after.get()),
            sql_number(through.get()),
            plan.map(|id| sql_number(id.get())),
            limit as i64,
            scope_sql.kind,
            scope_sql.keys_json
        ])
        .map_err(sql_error)?;
    let mut events = Vec::with_capacity(limit);
    while let Some(row) = rows.next().map_err(sql_error)? {
        events.push(event_row(row)?);
    }
    Ok(events)
}

pub(super) fn acknowledge(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    rendered_through: EventSeq,
) -> Result<BoardReply, BoardError> {
    if rendered_through.get() > max_seq(tx)?.get() {
        return Err(invalid(
            BoardErrorCode::InvalidReference,
            "inbox acknowledgement exceeds the latest event",
        ));
    }
    tx.execute(
        "UPDATE agent_sessions SET cursor_seq=max(coalesce(cursor_seq,0),?2) WHERE actor_id=?1",
        params![ctx.actor_id, sql_number(rendered_through.get())],
    )
    .map_err(sql_error)?;
    let cursor = read_cursor(tx, ctx.actor_id)?;
    Ok(BoardReply::new("local", BoardResult::Cursor(cursor)))
}

fn read_cursor(conn: &Connection, actor_id: i64) -> Result<EventSeq, BoardError> {
    let value: i64 = conn
        .query_row(
            "SELECT coalesce(cursor_seq,0) FROM agent_sessions WHERE actor_id=?1",
            [actor_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    Ok(EventSeq::new(sqlite_u64(value)?))
}
