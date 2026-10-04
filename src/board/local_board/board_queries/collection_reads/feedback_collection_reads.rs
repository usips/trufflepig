//! Bounded global feedback pages and metadata decoding for individual entries.

use rusqlite::{Connection, params};

use super::{COLLECTION_LIMIT, count, sequence_window, validate_limit};
use crate::board::board_domain::board_collections::{EntryCursor, FeedbackPage};
use crate::board::board_ids::EventSeq;
use crate::board::board_protocol::{
    BoardReply, BoardResult, EntryRecord, EntryState, FeedbackMetadata, FeedbackRecord,
};
use crate::board::board_vocabulary::FeedbackKind;
use crate::board::local_board::board_queries::board_reads;
use crate::board::local_board::{BoardError, invalid, sql_error, sql_number};

pub(in crate::board::local_board) fn feedback_page(
    conn: &Connection,
    open_only: bool,
    after: Option<EntryCursor>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    validate_limit(limit, COLLECTION_LIMIT)?;
    let (_, through) = sequence_window(conn, after.map(|cursor| cursor.seq), through)?;
    let predicate = concat!(
        "e.kind='feedback' AND (?1=0 OR e.state IN ('open','triaged')) ",
        "AND (e.seq>?2 OR (e.seq=?2 AND e.id>?3)) AND e.seq<=?4"
    );
    let parameters = params![
        open_only,
        sql_number(after.map_or(0, |cursor| cursor.seq.get())),
        sql_number(after.map_or(0, |cursor| cursor.entry.get())),
        sql_number(through.get())
    ];
    let total = count(
        conn,
        &format!(
            "SELECT count(*) FROM board_feedback f JOIN entries e ON e.id=f.entry_id WHERE {predicate}"
        ),
        parameters,
    )?;
    let mut entries = board_reads::entries(
        conn,
        &format!(
            concat!(
                "SELECT e.id FROM board_feedback f JOIN entries e ON e.id=f.entry_id WHERE {predicate} ",
                "ORDER BY e.seq,e.id LIMIT ?5"
            ),
            predicate = predicate
        ),
        params![
            open_only,
            sql_number(after.map_or(0, |cursor| cursor.seq.get())),
            sql_number(after.map_or(0, |cursor| cursor.entry.get())),
            sql_number(through.get()),
            sql_number((limit + 1) as u64)
        ],
    )?;
    let next_after = if entries.len() > limit {
        entries.truncate(limit);
        entries.last().map(|entry| EntryCursor {
            seq: entry.seq,
            entry: entry.id,
        })
    } else {
        None
    };
    let mut feedback = Vec::with_capacity(entries.len());
    for entry in entries {
        feedback.push(feedback_record(conn, entry)?);
    }
    Ok(BoardReply::new(
        "local",
        BoardResult::Feedback(FeedbackPage {
            open_only,
            omitted: total.saturating_sub(feedback.len()),
            feedback,
            after,
            through,
            next_after,
        }),
    ))
}

pub(in crate::board::local_board) fn feedback_record(
    conn: &Connection,
    entry: EntryRecord,
) -> Result<FeedbackRecord, BoardError> {
    let mut statement = conn.prepare(
        "SELECT feedback_kind,version,build_id,cwd,steer_mode,recent_calls_json FROM board_feedback WHERE entry_id=?1"
    ).map_err(sql_error)?;
    let mut rows = statement
        .query([sql_number(entry.id.get())])
        .map_err(sql_error)?;
    let row = rows.next().map_err(sql_error)?.ok_or_else(|| {
        invalid(
            "board_unavailable",
            format!("missing feedback metadata for {}", entry.id),
        )
    })?;
    let state = match entry.state {
        Some(EntryState::Feedback(state)) => state,
        _ => {
            return Err(invalid(
                "board_unavailable",
                "invalid stored feedback state",
            ));
        }
    };
    let kind = FeedbackKind::parse(&row.get::<_, String>(0).map_err(sql_error)?)
        .map_err(BoardError::from)?;
    let calls: String = row.get(5).map_err(sql_error)?;
    let metadata = FeedbackMetadata {
        version: row.get(1).map_err(sql_error)?,
        build_id: row.get(2).map_err(sql_error)?,
        repo_key: entry.repo_key.clone(),
        cwd: row.get(3).map_err(sql_error)?,
        steer_mode: row.get(4).map_err(sql_error)?,
        recent_calls: serde_json::from_str(&calls)
            .map_err(|error| invalid("board_unavailable", error.to_string()))?,
    };
    metadata
        .validate()
        .map_err(|error| invalid("board_unavailable", error.to_string()))?;
    Ok(FeedbackRecord {
        entry,
        kind,
        state,
        metadata,
    })
}
