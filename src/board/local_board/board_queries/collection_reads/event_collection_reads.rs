//! Event, revision, and entry pages with source-sequence keysets.

use rusqlite::{Connection, params};

use super::{COLLECTION_LIMIT, sequence_window, validate_limit};
use crate::board::board_actor::HarnessLabel;
use crate::board::board_domain::board_collections::{
    EntriesPage, EntryCursor, EventPage, HistoryPage, RevisionHistoryRecord,
};
use crate::board::board_ids::{EntryId, EventSeq, PlanId, PlanRevision, TaskId};
use crate::board::board_protocol::{BoardReply, BoardResult, RevisionSource};
use crate::board::board_vocabulary::{EntryKind, EntryText};
use crate::board::local_board::board_queries::board_reads;
use crate::board::local_board::{
    BoardError, actor_from_row, board_feed, invalid, require_plan, row_number, sql_error,
    sql_number,
};

const EVENT_LIMIT: usize = 500;

pub(in crate::board::local_board) fn feed(
    conn: &Connection,
    plan: Option<PlanId>,
    after: Option<EventSeq>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    validate_limit(limit, EVENT_LIMIT)?;
    if let Some(plan) = plan {
        require_plan(conn, plan)?;
    }
    let (after, through) = sequence_window(conn, after, through)?;
    let mut events = board_feed::read_events(conn, after, through, plan, limit + 1)?;
    let next_after = if events.len() > limit {
        events.truncate(limit);
        events.last().map(|event| event.seq)
    } else {
        None
    };
    Ok(BoardReply::new(
        "local",
        BoardResult::Feed(EventPage {
            plan,
            events,
            after,
            through,
            next_after,
        }),
    ))
}

pub(in crate::board::local_board) fn history(
    conn: &Connection,
    plan: PlanId,
    after: Option<EventSeq>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    validate_limit(limit, COLLECTION_LIMIT)?;
    require_plan(conn, plan)?;
    let (after, through) = sequence_window(conn, after, through)?;
    let mut statement = conn
        .prepare(concat!(
            "SELECT r.number,r.source,a.user,a.host,a.harness,a.session,e.body,r.seq,e.created_at ",
            "FROM revisions r JOIN actors a ON a.id=r.actor_id JOIN entries e ON e.id=r.entry_id ",
            "WHERE r.plan_id=?1 AND r.seq>?2 AND r.seq<=?3 ORDER BY r.seq,r.number LIMIT ?4"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(plan.get()),
            sql_number(after.get()),
            sql_number(through.get()),
            sql_number((limit + 1) as u64)
        ])
        .map_err(sql_error)?;
    let mut revisions = Vec::with_capacity(limit + 1);
    while let Some(row) = rows.next().map_err(sql_error)? {
        let source: String = row.get(1).map_err(sql_error)?;
        let source = RevisionSource::parse(&source)
            .map_err(|error| invalid("board_unavailable", error.to_string()))?;
        revisions.push(RevisionHistoryRecord {
            id: PlanRevision::new(plan, row_number(row, 0).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            source,
            actor: actor_from_row(row, 2).map_err(sql_error)?,
            summary: EntryText::new(row.get::<_, String>(6).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            seq: EventSeq::new(row_number(row, 7).map_err(sql_error)?),
            created_at: row.get(8).map_err(sql_error)?,
        });
    }
    let next_after = if revisions.len() > limit {
        revisions.truncate(limit);
        revisions.last().map(|revision| revision.seq)
    } else {
        None
    };
    Ok(BoardReply::new(
        "local",
        BoardResult::History(HistoryPage {
            plan,
            revisions,
            after,
            through,
            next_after,
        }),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::board::local_board) fn entries_page(
    conn: &Connection,
    plan: Option<PlanId>,
    kind: Option<EntryKind>,
    harness: Option<&HarnessLabel>,
    user: Option<&str>,
    host: Option<&str>,
    task: Option<TaskId>,
    references: Option<EntryId>,
    after: Option<EntryCursor>,
    before: Option<EntryCursor>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    validate_limit(limit, COLLECTION_LIMIT)?;
    if after.is_some() && before.is_some() {
        return Err(invalid(
            "invalid_options",
            "entries accepts at most one of after and before",
        ));
    }
    if let Some(plan) = plan {
        require_plan(conn, plan)?;
    }
    if let Some(task) = task {
        validate_task_filter(conn, plan, task)?;
    }
    if before.is_some_and(|cursor| through.is_some_and(|end| cursor.seq > end)) {
        return Err(invalid("invalid_reference", "before exceeds through"));
    }
    // `after` pages ascending (legacy); `before` and no cursor page
    // descending newest-first, so the first page needs no cursor at all.
    let descending = after.is_none();
    let (_, through) = sequence_window(conn, after.map(|cursor| cursor.seq), through)?;
    let task_ref = task.map(|task| task.to_string());
    let reference_target = references.map(|entry| entry.to_string());
    let cursor = after.or(before);
    let keyset = if descending {
        "AND (?7 IS NULL OR e.seq<?7 OR (e.seq=?7 AND e.id<?8)) "
    } else {
        "AND (e.seq>?7 OR (e.seq=?7 AND e.id>?8)) "
    };
    let ordering = if descending {
        "ORDER BY e.seq DESC,e.id DESC "
    } else {
        "ORDER BY e.seq,e.id "
    };
    let entries = board_reads::entries(
        conn,
        &format!(
            concat!(
                "SELECT e.id FROM entries e JOIN actors a ON a.id=e.actor_id ",
                "WHERE (?1 IS NULL OR e.plan_id=?1) AND (?2 IS NULL OR e.kind=?2) AND (?3 IS NULL OR a.harness=?3) ",
                "AND (?4 IS NULL OR a.user=?4) AND (?5 IS NULL OR a.host=?5) ",
                "AND (?6 IS NULL OR EXISTS(SELECT 1 FROM entry_refs reference ",
                "WHERE reference.entry_id=e.id AND reference.target=?6) ",
                "OR EXISTS(SELECT 1 FROM commit_plans link JOIN commit_tasks task ",
                "ON task.repo_key=link.repo_key AND task.oid=link.oid ",
                "AND task.plan_id=link.plan_id WHERE link.entry_id=e.id AND task.plan_id=?10 AND task.task_ordinal=?11)) ",
                "AND (?13 IS NULL OR EXISTS(SELECT 1 FROM entry_refs backref ",
                "WHERE backref.entry_id=e.id AND backref.target=?13)) ",
                "{keyset}AND e.seq<=?9 {ordering}LIMIT ?12"
            ),
            keyset = keyset,
            ordering = ordering
        ),
        params![
            plan.map(|plan| sql_number(plan.get())),
            kind.map(EntryKind::as_str),
            harness.map(HarnessLabel::as_str),
            user,
            host,
            task_ref,
            cursor.map(|cursor| sql_number(cursor.seq.get())),
            cursor.map(|cursor| sql_number(cursor.entry.get())),
            sql_number(through.get()),
            task.map(|task| sql_number(task.plan.get())),
            task.map(|task| sql_number(task.ordinal)),
            sql_number((limit + 1) as u64),
            reference_target
        ],
    )?;
    let mut entries = entries;
    let (next_after, next_before) = if entries.len() > limit {
        entries.truncate(limit);
        let cursor = entries.last().map(|entry| EntryCursor {
            seq: entry.seq,
            entry: entry.id,
        });
        if descending {
            (None, cursor)
        } else {
            (cursor, None)
        }
    } else {
        (None, None)
    };
    Ok(BoardReply::new(
        "local",
        BoardResult::Entries(EntriesPage {
            plan,
            references,
            entries,
            after,
            through,
            next_after,
            next_before,
        }),
    ))
}

fn validate_task_filter(
    conn: &Connection,
    plan: Option<PlanId>,
    task: TaskId,
) -> Result<(), BoardError> {
    task.validate().map_err(BoardError::from)?;
    if plan.is_some_and(|plan| plan != task.plan) {
        return Err(invalid(
            "invalid_reference",
            "task filter belongs to a different plan",
        ));
    }
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if !exists {
        return Err(invalid("invalid_reference", format!("unknown task {task}")));
    }
    Ok(())
}
