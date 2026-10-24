//! Feedback evidence, terminal triage, and permanent outbox replay identities.

mod feedback_imports;
mod feedback_transitions;

pub(in crate::board::local_board) use feedback_imports::{imported_reply, remember_import};
pub(in crate::board::local_board) use feedback_transitions::close_feedback;

use super::super::{
    EntryDraft, WriteContext, insert_entry, insert_event, invalid, read_entry, require_plan,
    sql_error, sqlite_id, sqlite_u64,
};
use crate::board::board_actor::{BoardActor, BoardRecipient};
use crate::board::board_ids::EntryId;
use crate::board::board_protocol::{
    AuthorityRole, AuthoritySource, BoardChange, BoardError, BoardOp, BoardReply, BoardResult,
    EntryRecord,
};
#[cfg(test)]
use crate::board::board_protocol::{FeedbackMetadata, FeedbackRecord};
#[cfg(test)]
use crate::board::board_vocabulary::FeedbackKind;
use crate::board::board_vocabulary::{
    EntryKind, FEEDBACK_TEXT_LIMIT, FeedbackImportKey, FeedbackState,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(in crate::board::local_board) fn write_feedback(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    op: &BoardOp,
) -> Result<BoardReply, BoardError> {
    let BoardOp::Feedback {
        kind,
        summary,
        body,
        plan,
        metadata,
        import_key,
    } = op
    else {
        return Err(invalid("invalid_options", "expected feedback operation"));
    };
    op.validate().map_err(BoardError::from)?;
    if let Some(key) = import_key {
        if let Some(reply) = imported_reply(tx, key)? {
            return Ok(reply);
        }
    }
    if let Some(plan) = plan {
        require_plan(tx, *plan)?;
    }
    if let Some(repo) = &metadata.repo_key {
        tx.execute(
            "INSERT OR IGNORE INTO repos(repo_key) VALUES(?1)",
            [repo.as_str()],
        )
        .map_err(sql_error)?;
    }
    let text = match body {
        Some(body) => format!("{}\n\n{}", summary.as_str(), body.as_str()),
        None => summary.as_str().to_owned(),
    };
    let report_ctx = ctx.with_authority(AuthoritySource::Reporter);
    let entry = insert_entry(
        tx,
        &report_ctx,
        &EntryDraft {
            plan_id: *plan,
            kind: EntryKind::Feedback,
            body: text,
            to_whom: None,
            supersedes: None,
            repo_key: metadata.repo_key.clone(),
            state: Some(FeedbackState::Open.as_str().to_owned()),
        },
    )?;
    if let Some(via) = ctx.via {
        tx.execute(
            "UPDATE entries SET via=?1 WHERE id=?2",
            params![via.as_str(), sqlite_id(entry.get())?],
        )
        .map_err(sql_error)?;
    }
    let recent_calls = serde_json::to_string(&metadata.recent_calls)
        .map_err(|error| invalid("invalid_options", error.to_string()))?;
    tx.execute(
        concat!(
            "INSERT INTO board_feedback(entry_id,feedback_kind,version,build_id,cwd,steer_mode,",
            "recent_calls_json,import_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)"
        ),
        params![
            sqlite_id(entry.get())?,
            kind.as_str(),
            metadata.version,
            metadata.build_id,
            metadata.cwd,
            metadata.steer_mode,
            recent_calls,
            import_key.map(|key| key.to_string())
        ],
    )
    .map_err(sql_error)?;
    if let Some(key) = import_key {
        remember_import(tx, key, entry)?;
    }
    insert_event(
        tx,
        &report_ctx,
        *plan,
        EntryKind::Feedback,
        &entry.to_string(),
        None,
        summary.as_str(),
    )?;
    Ok(ctx.change_reply(entry, *plan, None, None))
}

#[cfg(test)]
fn read_feedback(conn: &Connection, open_only: bool) -> Result<Vec<FeedbackRecord>, BoardError> {
    let mut statement = conn.prepare(
        concat!(
            "SELECT f.entry_id,f.feedback_kind,f.version,f.build_id,f.cwd,f.steer_mode,f.recent_calls_json,",
            "e.state FROM board_feedback f JOIN entries e ON e.id=f.entry_id ",
            "WHERE (?1=0 OR e.state IN ('open','triaged')) ORDER BY e.seq DESC"
        ),
    ).map_err(sql_error)?;
    let mut rows = statement.query([open_only]).map_err(sql_error)?;
    let mut records = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let entry_id =
            EntryId::new(sqlite_u64(row.get(0).map_err(sql_error)?)?).map_err(BoardError::from)?;
        let entry = read_entry(conn, entry_id)?;
        let kind: String = row.get(1).map_err(sql_error)?;
        let calls: String = row.get(6).map_err(sql_error)?;
        let state: String = row.get(7).map_err(sql_error)?;
        records.push(FeedbackRecord {
            metadata: FeedbackMetadata {
                version: row.get(2).map_err(sql_error)?,
                build_id: row.get(3).map_err(sql_error)?,
                repo_key: entry.repo_key.clone(),
                cwd: row.get(4).map_err(sql_error)?,
                steer_mode: row.get(5).map_err(sql_error)?,
                recent_calls: serde_json::from_str(&calls)
                    .map_err(|error| invalid("board_unavailable", error.to_string()))?,
            },
            entry,
            kind: FeedbackKind::parse(&kind).map_err(BoardError::from)?,
            state: FeedbackState::parse(&state).map_err(BoardError::from)?,
        });
    }
    Ok(records)
}

pub(in crate::board::local_board) fn can_manage_feedback(
    conn: &Connection,
    actor: &BoardActor,
    report: &EntryRecord,
    now: i64,
) -> Result<bool, BoardError> {
    Ok(feedback_authority(conn, actor, report, now)?.is_some())
}

pub(in crate::board::local_board) fn feedback_authority(
    conn: &Connection,
    actor: &BoardActor,
    report: &EntryRecord,
    now: i64,
) -> Result<Option<AuthoritySource>, BoardError> {
    if report.kind != EntryKind::Feedback {
        return Ok(None);
    }
    if actor == &report.actor {
        return Ok(Some(AuthoritySource::Reporter));
    }
    super::super::board_authority::authority_for_feedback(
        conn,
        actor,
        report.plan,
        &report.actor.user,
        AuthorityRole::Triage,
        now,
    )
}

fn truncate_entry_summary(text: &mut String) {
    if text.len() > FEEDBACK_TEXT_LIMIT {
        let mut end = FEEDBACK_TEXT_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

#[cfg(test)]
mod tests;
