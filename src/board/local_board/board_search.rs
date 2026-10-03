//! Searchable entry and revision targets with atomic external-content FTS maintenance.

#[cfg(test)]
mod tests;

use rusqlite::{Connection, params};

use super::{BoardError, invalid, require_plan, sql_error, sql_number, sqlite_u64};
use crate::board::board_ids::{BoardRef, PlanId};
use crate::board::board_protocol::{
    BoardReply, BoardResult, BoardSearchHit, BoardSearchReply, BoardSearchSource,
};

const SEARCH_LIMIT: usize = 50;
const SNIPPET_BYTES: usize = 512;

pub(super) fn search(
    conn: &Connection,
    query: &str,
    plan: Option<PlanId>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    if !(1..=SEARCH_LIMIT).contains(&limit)
        || query.trim().is_empty()
        || query.len() > 4096
        || query.contains('\0')
    {
        return Err(invalid(
            "invalid_options",
            "search requires a nonblank query of at most 4096 bytes and a limit of 1..50",
        ));
    }
    if let Some(plan) = plan {
        require_plan(conn, plan)?;
    }
    let mut statement = conn.prepare(
        "SELECT d.target,d.plan_id,d.source,substr(snippet(board_text,0,'','',' ... ',24),1,512) FROM board_text JOIN search_documents d ON d.rowid=board_text.rowid WHERE board_text MATCH ?1 AND (?2 IS NULL OR d.plan_id=?2) ORDER BY bm25(board_text),d.rowid LIMIT ?3",
    ).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            query,
            plan.map(|id| sql_number(id.get())),
            (limit + 1) as i64
        ])
        .map_err(match_error)?;
    let mut hits = Vec::with_capacity(limit + 1);
    while let Some(row) = rows.next().map_err(match_error)? {
        let target: BoardRef = row
            .get::<_, String>(0)
            .map_err(sql_error)?
            .parse()
            .map_err(BoardError::from)?;
        if !matches!(target, BoardRef::Entry(_) | BoardRef::Revision(_)) {
            return Err(invalid(
                "board_unavailable",
                "search document has an unsupported target",
            ));
        }
        let plan = row
            .get::<_, Option<i64>>(1)
            .map_err(sql_error)?
            .map(sqlite_u64)
            .transpose()?
            .map(PlanId::new)
            .transpose()
            .map_err(BoardError::from)?;
        let source = match row.get::<_, String>(2).map_err(sql_error)?.as_str() {
            "entry" => BoardSearchSource::Entry,
            "revision" => BoardSearchSource::Revision,
            "proposal" => BoardSearchSource::Proposal,
            _ => {
                return Err(invalid(
                    "board_unavailable",
                    "search document has an unsupported source",
                ));
            }
        };
        let mut snippet: String = row.get(3).map_err(sql_error)?;
        if snippet.len() > SNIPPET_BYTES {
            let mut end = SNIPPET_BYTES - 3;
            while !snippet.is_char_boundary(end) {
                end -= 1;
            }
            snippet.truncate(end);
            snippet.push_str("...");
        }
        hits.push(BoardSearchHit {
            target,
            plan,
            source,
            snippet,
        });
    }
    let truncated = hits.len() > limit;
    hits.truncate(limit);
    Ok(BoardReply::new(
        "local",
        BoardResult::Search(BoardSearchReply { hits, truncated }),
    ))
}

fn match_error(error: rusqlite::Error) -> BoardError {
    if let rusqlite::Error::SqliteFailure(code, Some(message)) = &error {
        if code.extended_code == rusqlite::ffi::SQLITE_ERROR
            && (message.starts_with("fts5:")
                || message.starts_with("unterminated string")
                || message.starts_with("no such column:")
                || message.starts_with("unknown special query:"))
        {
            return invalid(
                "invalid_options",
                format!("invalid search query: {message}"),
            );
        }
    }
    sql_error(error)
}
