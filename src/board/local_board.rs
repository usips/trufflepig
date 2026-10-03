//! SQLite-backed board state, serialized mutations, and immutable evidence.

mod board_database;
mod board_feed;
mod board_reads;
mod entry_writes;
mod feedback_entries;
mod plan_writes;
mod task_claims;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::board_actor::{BoardActor, HarnessLabel};
use super::board_backend::BoardBackend;
use super::board_config::BoardConfig;
use super::board_ids::{BoardRef, EntryId, EventSeq, PlanId, RepoKey, extract_refs};
use super::board_protocol::*;
use super::board_vocabulary::EntryKind;

pub struct LocalBoard {
    conn: Connection,
    path: PathBuf,
    claim_ttl_secs: i64,
    #[cfg(test)]
    panic_after_write: bool,
}

impl LocalBoard {
    pub fn open(config: &BoardConfig) -> Result<Self, BoardError> {
        Self::open_with_timeout(config, Duration::from_secs(5))
    }

    pub fn open_with_timeout(config: &BoardConfig, timeout: Duration) -> Result<Self, BoardError> {
        config.ensure_local().map_err(BoardError::from)?;
        Self::open_path_with_timeout(
            &config.db_path,
            Duration::from_secs(config.claim_ttl_seconds().cast_unsigned()),
            timeout,
        )
    }

    pub fn open_path(path: &Path, claim_ttl: Duration) -> Result<Self, BoardError> {
        Self::open_path_with_timeout(path, claim_ttl, Duration::from_secs(5))
    }

    pub fn open_path_with_timeout(
        path: &Path,
        claim_ttl: Duration,
        timeout: Duration,
    ) -> Result<Self, BoardError> {
        let (conn, path) = board_database::open_with_timeout(path, timeout)?;
        let claim_ttl_secs = i64::try_from(claim_ttl.as_secs())
            .map_err(|_| invalid("invalid_options", "claim TTL is too large"))?;
        if claim_ttl_secs == 0 {
            return Err(invalid("invalid_options", "claim TTL must be positive"));
        }
        Ok(Self {
            conn,
            path,
            claim_ttl_secs,
            #[cfg(test)]
            panic_after_write: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn inject_panic_after_write(&mut self) {
        self.panic_after_write = true;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_busy_timeout(&self, timeout: Duration) -> Result<(), BoardError> {
        self.conn
            .busy_timeout(timeout.min(Duration::from_secs(5)))
            .map_err(sql_error)
    }

    fn dispatch(&mut self, request: &BoardRequest) -> Result<BoardReply, BoardError> {
        request.validate().map_err(BoardError::from)?;
        #[cfg(unix)]
        if !request.op.is_read() {
            use std::os::unix::fs::PermissionsExt;
            let parent = self
                .path
                .parent()
                .ok_or_else(|| invalid("board_unavailable", "database has no parent"))?;
            if parent
                .metadata()
                .map_err(|e| invalid("board_unavailable", e.to_string()))?
                .permissions()
                .mode()
                & 0o200
                == 0
            {
                return Err(invalid(
                    "board_unavailable",
                    "database directory is not writable",
                ));
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let now = unix_now()?;
        let actor_id = ensure_actor(&tx, &request.actor, now)?;
        if let Some(claims) = &request.claims {
            tx.execute("UPDATE agent_sessions SET model=COALESCE(?2,model),effort=COALESCE(?3,effort) WHERE actor_id=?1", params![actor_id,claims.model,claims.effort]).map_err(sql_error)?;
        }
        let (model, effort): (Option<String>, Option<String>) = tx
            .query_row(
                "SELECT model,effort FROM agent_sessions WHERE actor_id=?1",
                [actor_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sql_error)?;
        let seq = max_seq(&tx)?
            .get()
            .checked_add(1)
            .ok_or_else(|| invalid("board_unavailable", "event sequence exhausted"))?;
        let key = request_dedupe_key(request)?;
        let ctx = WriteContext {
            actor_id,
            actor: request.actor.clone(),
            model,
            effort,
            now,
            seq: EventSeq::new(seq),
            claim_ttl_secs: self.claim_ttl_secs,
            dedupe_key: key.clone(),
        };
        let dedupable = is_dedupable(&request.op);
        let import_key = match &request.op {
            BoardOp::Feedback { import_key, .. } => import_key.as_deref(),
            _ => None,
        };
        if let Some(import_key) = import_key {
            if let Some(mut reply) = feedback_entries::imported_reply(&tx, import_key)? {
                if let BoardResult::Change(change) = &reply.result {
                    if let Some(plan) = change.plan {
                        task_claims::refresh_plan_claims(&tx, actor_id, plan, now)?;
                    }
                }
                reply.backend = format!("local:{}", self.path.display());
                tx.commit().map_err(sql_error)?;
                return Ok(reply);
            }
        }
        if dedupable {
            let stored: Option<String> = tx.query_row("SELECT reply_json FROM operation_dedupes WHERE dedupe_key=?1 AND created_at>=?2", params![key, now - 600], |r| r.get(0)).optional().map_err(sql_error)?;
            if let Some(stored) = stored {
                let mut reply: BoardReply = serde_json::from_str(&stored)
                    .map_err(|e| invalid("board_unavailable", e.to_string()))?;
                if receipt_current(&tx, request, &reply)? {
                    if let BoardResult::Change(change) = &mut reply.result {
                        change.deduplicated = true;
                        if let Some(import_key) = import_key {
                            feedback_entries::remember_import(&tx, import_key, change.entry)?;
                        }
                        if let Some(plan) = change.plan {
                            task_claims::refresh_plan_claims(&tx, actor_id, plan, now)?;
                        }
                    }
                    reply.backend = format!("local:{}", self.path.display());
                    tx.commit().map_err(sql_error)?;
                    return Ok(reply);
                }
                if matches!(request.op, BoardOp::CarveClaim { .. }) {
                    if let BoardResult::Change(change) = reply.result {
                        if let Some(task) = change.task {
                            if let BoardOp::CarveClaim { scope, .. } = &request.op {
                                let mut reply = task_claims::claim_task(&tx, &ctx, task, scope)?;
                                reply.backend = format!("local:{}", self.path.display());
                                task_claims::refresh_plan_claims(&tx, actor_id, task.plan, now)?;
                                tx.execute("UPDATE operation_dedupes SET reply_json=?2,created_at=?3 WHERE dedupe_key=?1",params![key,serde_json::to_string(&reply).map_err(|e|invalid("board_unavailable",e.to_string()))?,now]).map_err(sql_error)?;
                                tx.commit().map_err(sql_error)?;
                                return Ok(reply);
                            }
                        }
                    }
                }
            }
        }
        let mut reply = match &request.op {
            BoardOp::Hello { model, effort } => {
                entry_writes::hello(&tx, &ctx, model, effort.as_deref())?
            }
            BoardOp::Inbox { after, limit } => board_feed::inbox(&tx, &ctx, *after, *limit)?,
            BoardOp::AcknowledgeInbox { rendered_through } => {
                board_feed::acknowledge(&tx, &ctx, *rendered_through)?
            }
            BoardOp::Show { target } => board_reads::show(&tx, &ctx, target.as_ref())?,
            BoardOp::New {
                title,
                body,
                steward,
            } => plan_writes::new_plan(&tx, &ctx, title, body, steward.as_ref())?,
            BoardOp::Post {
                target,
                kind,
                body,
                to,
                supersedes,
            } => entry_writes::post(&tx, &ctx, target, *kind, body, to.as_ref(), *supersedes)?,
            BoardOp::TaskCreate {
                plan,
                title,
                to,
                section,
            } => {
                task_claims::create_task(&tx, &ctx, *plan, title, to.as_ref(), section.as_deref())?
            }
            BoardOp::TaskMove { task, column, to } => {
                task_claims::move_task(&tx, &ctx, *task, *column, to.as_ref())?
            }
            BoardOp::ClaimTask { task, scope } => task_claims::claim_task(&tx, &ctx, *task, scope)?,
            BoardOp::CarveClaim {
                plan,
                title,
                scope,
                section,
            } => task_claims::carve_claim(&tx, &ctx, *plan, title, scope, section.as_deref())?,
            BoardOp::Propose {
                base,
                body,
                summary,
            } => plan_writes::propose(&tx, &ctx, *base, body, summary)?,
            BoardOp::Edit {
                base,
                body,
                summary,
            } => plan_writes::edit(&tx, &ctx, *base, body, summary)?,
            BoardOp::Accept { proposal, note } => {
                plan_writes::accept(&tx, &ctx, *proposal, note.as_ref())?
            }
            BoardOp::Reject { proposal, reason } => {
                plan_writes::reject(&tx, &ctx, *proposal, reason)?
            }
            BoardOp::Review { base, agent } => {
                board_reads::review(&tx, &ctx, *base, agent.as_ref())?
            }
            BoardOp::Feedback { .. } => feedback_entries::write_feedback(&tx, &ctx, &request.op)?,
            BoardOp::FeedbackList { open_only } => {
                feedback_entries::list_feedback(&tx, *open_only)?
            }
            BoardOp::FeedbackClose { .. } => {
                feedback_entries::close_feedback(&tx, &ctx, &request.op)?
            }
            BoardOp::RegisterRepo { registration } => {
                entry_writes::register_repo(&tx, &ctx, registration)?
            }
            BoardOp::Repositories { plan } => board_reads::repositories(&tx, *plan)?,
            BoardOp::RecordScan {
                repo_key,
                host,
                common_dir,
                error,
            } => entry_writes::record_scan(&tx, repo_key, host, common_dir, error.as_deref())?,
            BoardOp::LinkCommits { commits } => entry_writes::link_commits(&tx, &ctx, commits)?,
        };
        #[cfg(test)]
        if std::mem::take(&mut self.panic_after_write) {
            panic!("injected panic after uncommitted board mutation");
        }
        if dedupable {
            let plan = match &reply.result {
                BoardResult::Change(change) => change.plan,
                _ => request.op.plan_id(),
            };
            if let Some(plan) = plan {
                task_claims::refresh_plan_claims(&tx, actor_id, plan, now)?;
            }
            let events: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM events WHERE seq=?1",
                    [sql_number(ctx.seq.get())],
                    |r| r.get(0),
                )
                .map_err(sql_error)?;
            let replay =
                matches!(&reply.result, BoardResult::Change(change) if change.deduplicated);
            if events != i64::from(!replay) {
                return Err(invalid(
                    "board_unavailable",
                    "mutation did not produce exactly one event",
                ));
            }
            reply.backend = format!("local:{}", self.path.display());
            let encoded = serde_json::to_string(&reply)
                .map_err(|e| invalid("board_unavailable", e.to_string()))?;
            tx.execute(
                "DELETE FROM operation_dedupes WHERE created_at<?1",
                [now - 600],
            )
            .map_err(sql_error)?;
            tx.execute("INSERT INTO operation_dedupes(dedupe_key,reply_json,created_at) VALUES(?1,?2,?3) ON CONFLICT(dedupe_key) DO UPDATE SET reply_json=excluded.reply_json,created_at=excluded.created_at", params![key, encoded, now]).map_err(sql_error)?;
        }
        reply.backend = format!("local:{}", self.path.display());
        tx.commit().map_err(sql_error)?;
        Ok(reply)
    }
}

impl BoardBackend for LocalBoard {
    fn handle(&mut self, request: &BoardRequest) -> Result<BoardReply, BoardError> {
        self.dispatch(request)
    }
    fn max_seq(&self) -> Result<EventSeq, BoardError> {
        max_seq(&self.conn)
    }
}

pub(super) struct WriteContext {
    pub actor_id: i64,
    pub actor: BoardActor,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub now: i64,
    pub seq: EventSeq,
    pub claim_ttl_secs: i64,
    pub dedupe_key: String,
}

pub(super) struct EntryDraft {
    pub plan_id: Option<PlanId>,
    pub kind: EntryKind,
    pub body: String,
    pub to_whom: Option<String>,
    pub supersedes: Option<EntryId>,
    pub repo_key: Option<RepoKey>,
    pub state: Option<String>,
    pub dedupe_key: Option<String>,
}

pub(super) fn insert_entry(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    entry: &EntryDraft,
) -> Result<EntryId, BoardError> {
    crate::board::board_vocabulary::EntryText::new(entry.body.clone()).map_err(BoardError::from)?;
    let id: u64 = tx.query_row("INSERT INTO entries(plan_id,kind,body,to_whom,supersedes,actor_id,model,effort,repo_key,state,dedupe_key,seq,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) RETURNING id", params![entry.plan_id.map(|id|sql_number(id.get())), entry.kind.as_str(), entry.body, entry.to_whom, entry.supersedes.map(|id|sql_number(id.get())), ctx.actor_id, ctx.model, ctx.effort, entry.repo_key.as_ref().map(RepoKey::as_str), entry.state, entry.dedupe_key.as_deref().unwrap_or(&ctx.dedupe_key), sql_number(ctx.seq.get()), ctx.now], |r| row_number(r,0)).map_err(sql_error)?;
    let id = EntryId::new(id).map_err(BoardError::from)?;
    for reference in extract_refs(&entry.body)
        .into_iter()
        .chain(entry.supersedes.map(BoardRef::Entry))
    {
        tx.execute(
            "INSERT OR IGNORE INTO entry_refs(entry_id,target) VALUES(?1,?2)",
            params![sql_number(id.get()), reference.to_string()],
        )
        .map_err(sql_error)?;
    }
    Ok(id)
}

pub(super) fn insert_event(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: Option<PlanId>,
    kind: &str,
    subject: &str,
    to: Option<&str>,
    summary: &str,
) -> Result<(), BoardError> {
    crate::board::board_vocabulary::EntryText::new(summary.to_owned()).map_err(BoardError::from)?;
    tx.execute("INSERT INTO events(seq,plan_id,kind,subject,to_whom,actor_id,summary,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![sql_number(ctx.seq.get()), plan.map(|id|sql_number(id.get())), kind, subject, to, ctx.actor_id, summary, ctx.now]).map_err(sql_error)?;
    Ok(())
}

pub(super) fn can_accept(
    conn: &Connection,
    actor: &BoardActor,
    plan: PlanId,
) -> Result<bool, BoardError> {
    let (owner, steward): (String, Option<String>) = conn
        .query_row(
            "SELECT owner_user,steward FROM plans WHERE id=?1",
            [sql_number(plan.get())],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown plan {plan}")))?;
    Ok(actor.user == owner
        && (actor.harness.as_str() == "human"
            || steward.as_deref() == Some(actor.harness.as_str())))
}

pub(super) fn require_plan(conn: &Connection, plan: PlanId) -> Result<(), BoardError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM plans WHERE id=?1)",
            [sql_number(plan.get())],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if exists {
        Ok(())
    } else {
        Err(invalid("invalid_reference", format!("unknown plan {plan}")))
    }
}

pub(super) fn actor_from_row(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<BoardActor> {
    let harness: String = row.get(offset + 2)?;
    let harness = HarnessLabel::parse(&harness).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(offset + 2, rusqlite::types::Type::Text, e.into())
    })?;
    Ok(BoardActor {
        user: row.get(offset)?,
        host: row.get(offset + 1)?,
        harness,
        session: row.get(offset + 3)?,
    })
}

pub(super) fn read_entry(conn: &Connection, id: EntryId) -> Result<EntryRecord, BoardError> {
    board_reads::entry(conn, id)
}

pub(super) fn sql_error(error: rusqlite::Error) -> BoardError {
    if matches!(&error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked))
    {
        BoardError::new(BoardErrorCode::DatabaseLocked, error.to_string())
    } else {
        invalid("board_unavailable", error.to_string())
    }
}
pub(super) fn invalid(code: &str, message: impl Into<String>) -> BoardError {
    BoardError::from(anyhow::anyhow!("{code}: {}", message.into()))
}
pub(super) fn max_seq(conn: &Connection) -> Result<EventSeq, BoardError> {
    let seq: u64 = conn
        .query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| {
            row_number(r, 0)
        })
        .map_err(sql_error)?;
    Ok(EventSeq::new(seq))
}
pub(super) fn unix_now() -> Result<i64, BoardError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| invalid("board_unavailable", e.to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|e| invalid("board_unavailable", e.to_string()))
}

fn ensure_actor(tx: &Transaction<'_>, actor: &BoardActor, now: i64) -> Result<i64, BoardError> {
    tx.execute(
        "INSERT OR IGNORE INTO actors(user,host,harness,session) VALUES(?1,?2,?3,?4)",
        params![
            actor.user,
            actor.host,
            actor.harness.as_str(),
            actor.session
        ],
    )
    .map_err(sql_error)?;
    let id = tx
        .query_row(
            "SELECT id FROM actors WHERE user=?1 AND host=?2 AND harness=?3 AND session=?4",
            params![
                actor.user,
                actor.host,
                actor.harness.as_str(),
                actor.session
            ],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    tx.execute("INSERT INTO agent_sessions(actor_id,first_seen,last_seen) VALUES(?1,?2,?2) ON CONFLICT(actor_id) DO UPDATE SET last_seen=excluded.last_seen", params![id,now]).map_err(sql_error)?;
    Ok(id)
}

fn is_dedupable(op: &BoardOp) -> bool {
    matches!(
        op,
        BoardOp::Hello { .. }
            | BoardOp::New { .. }
            | BoardOp::Post { .. }
            | BoardOp::TaskCreate { .. }
            | BoardOp::TaskMove { .. }
            | BoardOp::ClaimTask { .. }
            | BoardOp::CarveClaim { .. }
            | BoardOp::Propose { .. }
            | BoardOp::Edit { .. }
            | BoardOp::Accept { .. }
            | BoardOp::Reject { .. }
            | BoardOp::Feedback { .. }
            | BoardOp::FeedbackClose { .. }
    )
}

fn request_dedupe_key(request: &BoardRequest) -> Result<String, BoardError> {
    let mut canonical = request.clone();
    if let BoardOp::Feedback { import_key, .. } = &mut canonical.op {
        *import_key = None;
    }
    let bytes =
        serde_json::to_vec(&canonical).map_err(|e| invalid("board_unavailable", e.to_string()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(super) fn sqlite_id(value: u64) -> Result<i64, BoardError> {
    i64::try_from(value)
        .map_err(|_| invalid("invalid_reference", "number exceeds SQLite integer range"))
}

pub(super) fn sqlite_u64(value: i64) -> Result<u64, BoardError> {
    u64::try_from(value).map_err(|_| invalid("board_unavailable", "negative stored number"))
}

pub(super) struct SqlU64(u64);

pub(super) fn sql_number(value: u64) -> SqlU64 {
    SqlU64(value)
}

impl rusqlite::ToSql for SqlU64 {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        let number = i64::try_from(self.0)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok(rusqlite::types::ToSqlOutput::Owned(number.into()))
    }
}

pub(super) fn row_number(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(e),
        )
    })
}

fn receipt_current(
    tx: &Transaction<'_>,
    request: &BoardRequest,
    reply: &BoardReply,
) -> Result<bool, BoardError> {
    match &request.op {
        BoardOp::ClaimTask { .. } | BoardOp::CarveClaim { .. } => {
            let BoardResult::Change(change) = &reply.result else {
                return Ok(false);
            };
            let Some(task) = change.task else {
                return Ok(false);
            };
            tx.query_row("SELECT EXISTS(SELECT 1 FROM claims c JOIN actors a ON a.id=c.actor_id WHERE c.plan_id=?1 AND c.task_ordinal=?2 AND c.entry_id=?3 AND c.ended_at IS NULL AND a.user=?4 AND a.host=?5 AND a.harness=?6 AND a.session=?7)",params![sql_number(task.plan.get()),sql_number(task.ordinal),sql_number(change.entry.get()),request.actor.user,request.actor.host,request.actor.harness.as_str(),request.actor.session],|r|r.get(0)).map_err(sql_error)
        }
        BoardOp::TaskMove { task, .. } => {
            let BoardResult::Change(change) = &reply.result else {
                return Ok(false);
            };
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2 AND seq=?3)",
                params![
                    sql_number(task.plan.get()),
                    sql_number(task.ordinal),
                    sql_number(change.seq.get())
                ],
                |r| r.get(0),
            )
            .map_err(sql_error)
        }
        BoardOp::Hello { model, effort } => {
            let (current_model,current_effort):(Option<String>,Option<String>) = tx.query_row("SELECT s.model,s.effort FROM agent_sessions s JOIN actors a ON a.id=s.actor_id WHERE a.user=?1 AND a.host=?2 AND a.harness=?3 AND a.session=?4",params![request.actor.user,request.actor.host,request.actor.harness.as_str(),request.actor.session],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sql_error)?;
            Ok(current_model.as_deref() == Some(model.as_str()) && current_effort == *effort)
        }
        _ => Ok(true),
    }
}
