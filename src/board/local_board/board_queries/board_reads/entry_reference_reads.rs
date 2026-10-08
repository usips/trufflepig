//! Stored entries, references, and actionable entry views.
mod reminder_reads;

use super::*;
use crate::board::board_actor::claim_vendor;
pub(in crate::board::local_board) use reminder_reads::open_entries;
#[cfg(test)]
pub(super) use reminder_reads::reminder_predicate;

pub(in crate::board::local_board) fn entry(
    conn: &Connection,
    id: EntryId,
) -> Result<EntryRecord, BoardError> {
    let mut statement = conn.prepare(
        concat!(
            "SELECT e.plan_id,e.kind,e.body,e.to_whom,e.supersedes,a.user,a.host,a.harness,a.session,",
            "e.model,e.effort,e.repo_key,coalesce(p.state,e.state),e.seq,e.created_at,e.via ",
            "FROM entries e JOIN actors a ON a.id=e.actor_id LEFT JOIN proposals p ON p.entry_id=e.id WHERE e.id=?1"
        ),
    ).map_err(sql_error)?;
    let mut rows = statement.query([sql_number(id.get())]).map_err(sql_error)?;
    let row = rows
        .next()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown entry {id}")))?;
    let kind: String = row.get(1).map_err(sql_error)?;
    let kind: EntryKind = kind.parse().map_err(BoardError::from)?;
    let to: Option<String> = row.get(3).map_err(sql_error)?;
    let repo_key: Option<String> = row.get(11).map_err(sql_error)?;
    let state: Option<String> = row.get(12).map_err(sql_error)?;
    let state = match (kind, state.as_deref()) {
        (EntryKind::Feedback, Some(state)) => Some(EntryState::Feedback(
            state.parse().map_err(BoardError::from)?,
        )),
        (EntryKind::Proposal, Some(state)) => Some(EntryState::Proposal(
            state.parse().map_err(BoardError::from)?,
        )),
        (_, None) => None,
        _ => {
            return Err(invalid(
                "board_unavailable",
                format!("invalid stored state for {id}"),
            ));
        }
    };
    let actor = actor_from_row(row, 5).map_err(sql_error)?;
    let model: Option<String> = row.get(9).map_err(sql_error)?;
    let mut record = EntryRecord {
        id,
        via: row
            .get::<_, Option<String>>(15)
            .map_err(sql_error)?
            .as_deref()
            .map(FeedbackVia::parse)
            .transpose()
            .map_err(|error| invalid("board_unavailable", error.to_string()))?,
        plan: row
            .get::<_, Option<i64>>(0)
            .map_err(sql_error)?
            .map(sqlite_u64)
            .transpose()?
            .map(PlanId::new)
            .transpose()
            .map_err(BoardError::from)?,
        kind,
        body: EntryText::new(row.get::<_, String>(2).map_err(sql_error)?)
            .map_err(BoardError::from)?,
        to: to
            .as_deref()
            .map(BoardRecipient::parse)
            .transpose()
            .map_err(BoardError::from)?,
        supersedes: row
            .get::<_, Option<i64>>(4)
            .map_err(sql_error)?
            .map(sqlite_u64)
            .transpose()?
            .map(EntryId::new)
            .transpose()
            .map_err(BoardError::from)?,
        vendor: claim_vendor(&actor.harness, model.as_deref()),
        actor,
        model,
        effort: row.get(10).map_err(sql_error)?,
        repo_key: repo_key
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(BoardError::from)?,
        state,
        refs: Vec::new(),
        seq: EventSeq::new(row_number(row, 13).map_err(sql_error)?),
        created_at: row.get(14).map_err(sql_error)?,
    };
    let mut references = conn
        .prepare("SELECT target FROM entry_refs WHERE entry_id=?1 ORDER BY target")
        .map_err(sql_error)?;
    let mut rows = references
        .query([sql_number(id.get())])
        .map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        record.refs.push(
            row.get::<_, String>(0)
                .map_err(sql_error)?
                .parse()
                .map_err(BoardError::from)?,
        );
    }
    Ok(record)
}

pub(in crate::board::local_board) fn entries(
    conn: &Connection,
    sql: &str,
    parameters: impl Params,
) -> Result<Vec<EntryRecord>, BoardError> {
    let mut statement = conn.prepare(sql).map_err(sql_error)?;
    let ids = statement
        .query_map(parameters, |row| row_number(row, 0))
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    ids.into_iter()
        .map(|id| entry(conn, EntryId::new(id).map_err(BoardError::from)?))
        .collect()
}

pub(in crate::board::local_board) fn entry_view(
    conn: &Connection,
    ctx: &WriteContext,
    id: EntryId,
) -> Result<EntryView, BoardError> {
    let entry = entry(conn, id)?;
    let reference = id.to_string();
    let read_backrefs = |answers_only: bool| -> Result<(Vec<EntryRecord>, usize), BoardError> {
        let kind = if answers_only {
            " AND e.kind='answer'"
        } else {
            ""
        };
        let predicate =
            format!("entry_refs r JOIN entries e ON e.id=r.entry_id WHERE r.target=?1{kind}");
        let total: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {predicate}"),
                [&reference],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        let records = entries(
            conn,
            &format!("SELECT e.id FROM {predicate} ORDER BY e.seq,e.id LIMIT 20"),
            [&reference],
        )?;
        let omitted = usize::try_from(total)
            .map_err(|_| invalid("board_unavailable", "invalid backreference count"))?
            .saturating_sub(records.len());
        Ok((records, omitted))
    };
    let (replies, replies_omitted) = read_backrefs(true)?;
    let (backrefs, backrefs_omitted) = read_backrefs(false)?;
    let mut statement = conn
        .prepare(concat!(
            "SELECT p.plan_id,p.base_revision,t.body,p.state,p.decision_entry,p.result_revision,",
            "p.base_revision<plan.head_revision FROM proposals p JOIN texts t ON t.hash=p.text_hash ",
            "JOIN plans plan ON plan.id=p.plan_id WHERE p.entry_id=?1"
        ))
        .map_err(sql_error)?;
    let mut rows = statement.query([sql_number(id.get())]).map_err(sql_error)?;
    let proposal = if let Some(row) = rows.next().map_err(sql_error)? {
        Some(ProposalRecord {
            entry: id,
            plan: PlanId::new(row_number(row, 0).map_err(sql_error)?).map_err(BoardError::from)?,
            base_revision: row_number(row, 1).map_err(sql_error)?,
            body: PlanText::new(row.get::<_, String>(2).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            state: row
                .get::<_, String>(3)
                .map_err(sql_error)?
                .parse()
                .map_err(BoardError::from)?,
            decision_entry: row
                .get::<_, Option<i64>>(4)
                .map_err(sql_error)?
                .map(sqlite_u64)
                .transpose()?
                .map(EntryId::new)
                .transpose()
                .map_err(BoardError::from)?,
            result_revision: row
                .get::<_, Option<i64>>(5)
                .map_err(sql_error)?
                .map(sqlite_u64)
                .transpose()?,
            stale_base: row.get(6).map_err(sql_error)?,
        })
    } else {
        None
    };
    let authority = entry
        .plan
        .map(|plan| crate::board::local_board::can_accept(conn, &ctx.actor, plan))
        .transpose()?
        .unwrap_or(false);
    let can_decide = authority
        && proposal
            .as_ref()
            .is_some_and(|proposal| proposal.state == ProposalState::Open);
    let can_supersede = authority
        || (entry.actor.user == ctx.actor.user && entry.actor.harness == ctx.actor.harness);
    let can_answer = conn
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM entries e WHERE e.id=?1 AND ({OPEN_QUESTION}))"),
            [sql_number(id.get())],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let feedback_authority =
        crate::board::local_board::board_writes::feedback_entries::can_manage_feedback(
            conn, &ctx.actor, &entry,
        )?;
    let can_triage = feedback_authority
        && matches!(
            entry.state,
            Some(EntryState::Feedback(
                crate::board::board_vocabulary::FeedbackState::Open
            ))
        );
    let can_close = feedback_authority
        && matches!(entry.state, Some(EntryState::Feedback(state)) if !state.is_closed());
    let plan_head_revision = entry
        .plan
        .map(|id| plan(conn, id).map(|record| record.head_revision))
        .transpose()?;
    let feedback = if entry.kind == EntryKind::Feedback {
        Some(
            crate::board::local_board::board_queries::collection_reads::feedback_record(
                conn,
                entry.clone(),
            )?,
        )
    } else {
        None
    };
    let linked_commit = linked_commit_for_entry(conn, id)?;
    let cursor = |records: &[EntryRecord], omitted: usize| {
        if omitted == 0 {
            None
        } else {
            records.last().map(|record| EntryCursor {
                seq: record.seq,
                entry: record.id,
            })
        }
    };
    let replies_next_after = cursor(&replies, replies_omitted);
    let backrefs_next_after = cursor(&backrefs, backrefs_omitted);
    Ok(EntryView {
        entry,
        replies,
        replies_omitted,
        backrefs,
        backrefs_omitted,
        replies_next_after,
        backrefs_next_after,
        through: crate::board::local_board::max_seq(conn)?,
        proposal,
        feedback,
        linked_commit,
        can_decide,
        can_supersede,
        can_answer,
        can_triage,
        can_close,
        plan_head_revision,
    })
}
