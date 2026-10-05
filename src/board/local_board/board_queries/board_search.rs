//! Searchable entry, revision, and plan-title targets with atomic external-content FTS maintenance.

#[cfg(test)]
mod tests;

use rusqlite::{Connection, params};

use super::super::{BoardError, invalid, require_plan, sql_error, sql_number, sqlite_u64};
use crate::board::board_ids::{BoardRef, PlanId};
use crate::board::board_protocol::{
    BoardReply, BoardResult, BoardSearchHit, BoardSearchReply, BoardSearchSource,
};

const SEARCH_LIMIT: usize = 50;
const SNIPPET_BYTES: usize = 512;

pub(in crate::board::local_board) fn search(
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
    let expression = match_expression(query)?;
    // bm25 is computed per FTS table, so title and body ranks share a scale
    // in name only; the UNION orders by them anyway with a target tiebreak.
    // The result is deterministic but not a cross-source relevance claim.
    // Title targets (P#) never collide with body targets (E#, P#@N), so
    // UNION ALL skips the dedup pass; LIMIT applies across the UNION.
    let mut statement = conn.prepare(
        concat!(
            "SELECT target,plan_id,source,snippet FROM(",
            "SELECT d.target AS target,d.plan_id AS plan_id,d.source AS source,",
            "substr(snippet(board_text,0,'','',' ... ',24),1,512) AS snippet,bm25(board_text) AS rank ",
            "FROM board_text JOIN search_documents d ON d.rowid=board_text.rowid ",
            "WHERE board_text MATCH ?1 AND (?2 IS NULL OR d.plan_id=?2) UNION ALL ",
            "SELECT 'P'||t.plan_id,t.plan_id,'plan',substr(t.title,1,512),bm25(plan_titles) ",
            "FROM plan_titles JOIN plan_text t ON t.plan_id=plan_titles.rowid ",
            "WHERE plan_titles MATCH ?1 AND (?2 IS NULL OR t.plan_id=?2) ",
            "ORDER BY rank,target LIMIT ?3)"
        ),
    ).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            expression,
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
        if !matches!(
            target,
            BoardRef::Entry(_) | BoardRef::Revision(_) | BoardRef::Plan(_)
        ) {
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
            "plan" => BoardSearchSource::Plan,
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

/// Builds the FTS5 MATCH expression: kept terms are phrase-quoted (`"` doubled)
/// and ANDed so user text never parses as FTS5 operators. Terms without an
/// alphanumeric are dropped; one trailing `*` marks a phrase-prefix query.
fn match_expression(query: &str) -> Result<String, BoardError> {
    let mut expression = String::with_capacity(query.len() + 8);
    let mut kept = 0;
    for term in query.split_whitespace() {
        let (stem, prefix) = term
            .strip_suffix('*')
            .map_or((term, false), |stem| (stem, true));
        if !stem.chars().any(|character| character.is_alphanumeric()) {
            continue;
        }
        if kept > 0 {
            expression.push_str(" AND ");
        }
        kept += 1;
        expression.push('"');
        for character in stem.chars() {
            if character == '"' {
                expression.push('"');
            }
            expression.push(character);
        }
        expression.push('"');
        if prefix {
            expression.push('*');
        }
    }
    if kept == 0 {
        return Err(invalid(
            "invalid_options",
            "search query has no searchable terms",
        ));
    }
    Ok(expression)
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
