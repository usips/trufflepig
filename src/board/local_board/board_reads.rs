//! Typed immutable plan history, entry references, and review evidence.

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use rusqlite::{Connection, Params, Row, Transaction, params};

use super::{
    BoardError, WriteContext, actor_from_row, invalid, require_plan, row_number, sql_error,
    sql_number, sqlite_u64, task_claims,
};
use crate::board::board_actor::{BoardActor, BoardRecipient, HarnessLabel};
use crate::board::board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision, RepoKey};
use crate::board::board_protocol::*;
use crate::board::board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle, ProposalState};

const RECENT_ENTRIES: i64 = 20;
const PLAN_SELECT: &str = "SELECT id,title,owner_user,steward,head_revision,created_at FROM plans";
const OPEN_QUESTION: &str = "e.kind='question' AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs reference ON reference.entry_id=answer.id WHERE answer.kind='answer' AND reference.target='E'||e.id) AND NOT EXISTS(SELECT 1 FROM entries correction WHERE correction.supersedes=e.id)";

pub(super) fn show(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    target: Option<&BoardRef>,
) -> Result<BoardReply, BoardError> {
    let result = match target {
        None => BoardResult::Plans(plans(tx)?),
        Some(BoardRef::Plan(plan)) => BoardResult::Plan(plan_view(tx, ctx, *plan)?),
        Some(BoardRef::Task(task)) => {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
                    params![sql_number(task.plan.get()), sql_number(task.ordinal)],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if !exists {
                return Err(invalid("invalid_reference", format!("unknown task {task}")));
            }
            BoardResult::Plan(plan_view(tx, ctx, task.plan)?)
        }
        Some(BoardRef::Revision(id)) => BoardResult::Revision(revision(tx, *id)?),
        Some(BoardRef::Span(span)) => {
            let plan = plan(tx, span.plan)?;
            let end = span.end.unwrap_or(plan.head_revision);
            if end < span.start {
                return Err(invalid("invalid_reference", "revision range is reversed"));
            }
            BoardResult::Diff(RevisionDiff {
                before: revision(
                    tx,
                    PlanRevision::new(span.plan, span.start).map_err(BoardError::from)?,
                )?,
                after: revision(
                    tx,
                    PlanRevision::new(span.plan, end).map_err(BoardError::from)?,
                )?,
            })
        }
        Some(_) => {
            return Err(invalid(
                "invalid_reference",
                "show requires a plan, task, revision, or revision span",
            ));
        }
    };
    Ok(BoardReply::new("local", result))
}

fn plans(conn: &Connection) -> Result<Vec<PlanRecord>, BoardError> {
    let mut statement = conn
        .prepare(&format!("{PLAN_SELECT} ORDER BY id"))
        .map_err(sql_error)?;
    let mut rows = statement.query([]).map_err(sql_error)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        result.push(plan_row(row)?);
    }
    Ok(result)
}

fn plan(conn: &Connection, id: PlanId) -> Result<PlanRecord, BoardError> {
    let mut statement = conn
        .prepare(&format!("{PLAN_SELECT} WHERE id=?1"))
        .map_err(sql_error)?;
    let mut rows = statement.query([sql_number(id.get())]).map_err(sql_error)?;
    let row = rows
        .next()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown plan {id}")))?;
    plan_row(row)
}

fn plan_row(row: &Row<'_>) -> Result<PlanRecord, BoardError> {
    let steward: Option<String> = row.get(3).map_err(sql_error)?;
    Ok(PlanRecord {
        id: PlanId::new(row_number(row, 0).map_err(sql_error)?).map_err(BoardError::from)?,
        title: PlanTitle::new(row.get::<_, String>(1).map_err(sql_error)?)
            .map_err(BoardError::from)?,
        owner_user: row.get(2).map_err(sql_error)?,
        steward: steward
            .as_deref()
            .map(HarnessLabel::parse)
            .transpose()
            .map_err(BoardError::from)?,
        head_revision: row_number(row, 4).map_err(sql_error)?,
        created_at: row.get(5).map_err(sql_error)?,
    })
}

fn revision(conn: &Connection, id: PlanRevision) -> Result<RevisionRecord, BoardError> {
    let mut statement = conn.prepare(
        "SELECT t.body,r.source,r.entry_id,a.user,a.host,a.harness,a.session,r.seq,e.created_at FROM revisions r JOIN texts t ON t.hash=r.text_hash JOIN actors a ON a.id=r.actor_id JOIN entries e ON e.id=r.entry_id WHERE r.plan_id=?1 AND r.number=?2",
    ).map_err(sql_error)?;
    let mut rows = statement
        .query(params![sql_number(id.plan.get()), sql_number(id.revision)])
        .map_err(sql_error)?;
    let row = rows
        .next()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown revision {id}")))?;
    let source: String = row.get(1).map_err(sql_error)?;
    let source = match source.as_str() {
        "create" => RevisionSource::Create,
        "accept" => RevisionSource::Accept,
        "direct" => RevisionSource::Direct,
        _ => {
            return Err(invalid(
                "board_unavailable",
                format!("invalid stored revision source {source}"),
            ));
        }
    };
    Ok(RevisionRecord {
        id,
        body: PlanText::new(row.get::<_, String>(0).map_err(sql_error)?)
            .map_err(BoardError::from)?,
        source,
        entry: EntryId::new(row_number(row, 2).map_err(sql_error)?).map_err(BoardError::from)?,
        actor: actor_from_row(row, 3).map_err(sql_error)?,
        seq: EventSeq::new(row_number(row, 7).map_err(sql_error)?),
        created_at: row.get(8).map_err(sql_error)?,
    })
}

fn plan_view(conn: &Connection, ctx: &WriteContext, id: PlanId) -> Result<PlanView, BoardError> {
    let plan = plan(conn, id)?;
    let revision = revision(
        conn,
        PlanRevision::new(id, plan.head_revision).map_err(BoardError::from)?,
    )?;
    let tasks = task_claims::read_tasks(conn, id)?;
    let claims = task_claims::read_claims(conn, id, ctx.now, ctx.claim_ttl_secs)?
        .into_iter()
        .filter(|claim| claim.ended_at.is_none())
        .collect();
    let entries = entries(
        conn,
        &format!(
            "SELECT e.id FROM entries e WHERE e.plan_id=?1 AND (e.id IN (SELECT id FROM entries WHERE plan_id=?1 ORDER BY seq DESC,id DESC LIMIT ?2) OR EXISTS(SELECT 1 FROM proposals p WHERE p.entry_id=e.id AND p.state='open') OR (e.kind='feedback' AND e.state IN ('open','triaged')) OR ({OPEN_QUESTION})) ORDER BY e.seq,e.id"
        ),
        params![sql_number(id.get()), RECENT_ENTRIES],
    )?;
    let sections_without_tasks = uncovered_sections(revision.body.as_str(), &tasks);
    Ok(PlanView {
        plan,
        revision,
        tasks,
        claims,
        entries,
        sections_without_tasks,
    })
}

fn uncovered_sections(body: &str, tasks: &[TaskRecord]) -> Vec<String> {
    let mut sections = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    for line in body.lines() {
        let line = line.trim_start();
        let marker = line.as_bytes().first().copied();
        if matches!(marker, Some(b'`' | b'~')) {
            let marker = marker.unwrap_or_default();
            let width = line.bytes().take_while(|byte| *byte == marker).count();
            if width >= 3 {
                match fence {
                    None => fence = Some((marker, width)),
                    Some((open, size)) if open == marker && width >= size => fence = None,
                    Some(_) => {}
                }
                continue;
            }
        }
        if fence.is_some() {
            continue;
        }
        let width = line.bytes().take_while(|byte| *byte == b'#').count();
        if !(1..=6).contains(&width)
            || !line
                .as_bytes()
                .get(width)
                .is_some_and(u8::is_ascii_whitespace)
        {
            continue;
        }
        let heading = line[width..].trim().trim_end_matches('#').trim_end();
        if heading.is_empty()
            || tasks
                .iter()
                .any(|task| task.section.as_deref() == Some(heading))
            || sections.iter().any(|section| section == heading)
        {
            continue;
        }
        sections.push(heading.to_owned());
    }
    sections
}

pub(super) fn entry(conn: &Connection, id: EntryId) -> Result<EntryRecord, BoardError> {
    let mut statement = conn.prepare(
        "SELECT e.plan_id,e.kind,e.body,e.to_whom,e.supersedes,a.user,a.host,a.harness,a.session,e.model,e.effort,e.repo_key,coalesce(p.state,e.state),e.seq,e.created_at FROM entries e JOIN actors a ON a.id=e.actor_id LEFT JOIN proposals p ON p.entry_id=e.id WHERE e.id=?1",
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
    let mut record = EntryRecord {
        id,
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
        actor: actor_from_row(row, 5).map_err(sql_error)?,
        model: row.get(9).map_err(sql_error)?,
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

pub(super) fn entries(
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

pub(super) fn open_entries(
    conn: &Connection,
    actor: &BoardActor,
) -> Result<Vec<EntryRecord>, BoardError> {
    entries(
        conn,
        &format!(
            "SELECT e.id FROM entries e WHERE EXISTS(SELECT 1 FROM proposals p WHERE p.entry_id=e.id AND p.state='open') OR (e.kind='feedback' AND e.state IN ('open','triaged')) OR (({OPEN_QUESTION}) AND (e.to_whom IS NULL OR e.to_whom IN (?1,?2,?3))) ORDER BY e.seq,e.id"
        ),
        params![actor.user, actor.harness.as_str(), actor.identity()],
    )
}

pub(super) fn review(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    base: PlanRevision,
    agent: Option<&HarnessLabel>,
) -> Result<BoardReply, BoardError> {
    let plan = plan(tx, base.plan)?;
    let base = revision(tx, base)?;
    let head = revision(
        tx,
        PlanRevision::new(plan.id, plan.head_revision).map_err(BoardError::from)?,
    )?;
    let entries = entries(
        tx,
        "SELECT e.id FROM entries e JOIN actors a ON a.id=e.actor_id WHERE e.plan_id=?1 AND e.seq>?2 AND (?3 IS NULL OR a.harness=?3) ORDER BY e.seq,e.id",
        params![
            sql_number(plan.id.get()),
            sql_number(base.seq.get()),
            agent.map(HarnessLabel::as_str)
        ],
    )?;
    let claims = task_claims::read_claims_window(
        tx,
        plan.id,
        base.created_at,
        ctx.now,
        ctx.now,
        ctx.claim_ttl_secs,
    )?;
    let open_questions = entries_for_open_questions(tx, plan.id)?;
    let open_feedback = feedback_for_plan(tx, plan.id)?;
    let evidence = ReviewEvidence {
        agent: agent.cloned(),
        window_end: ctx.now,
        tasks: task_claims::read_tasks(tx, plan.id)?,
        claims,
        entries,
        commits: linked_commits(tx, plan.id, base.created_at, ctx.now)?,
        open_proposals: open_proposals(tx, plan.id)?,
        open_questions,
        open_feedback,
        plan,
        base,
        head,
    };
    Ok(BoardReply::new("local", BoardResult::Review(evidence)))
}

fn entries_for_open_questions(
    conn: &Connection,
    plan: PlanId,
) -> Result<Vec<EntryRecord>, BoardError> {
    entries(
        conn,
        &format!(
            "SELECT e.id FROM entries e WHERE e.plan_id=?1 AND ({OPEN_QUESTION}) ORDER BY e.seq,e.id"
        ),
        [sql_number(plan.get())],
    )
}

fn open_proposals(conn: &Connection, plan: PlanId) -> Result<Vec<ProposalRecord>, BoardError> {
    let mut statement = conn.prepare("SELECT p.entry_id,p.base_revision,t.body,p.state,p.decision_entry,p.result_revision FROM proposals p JOIN texts t ON t.hash=p.text_hash WHERE p.plan_id=?1 AND p.state='open' ORDER BY p.entry_id").map_err(sql_error)?;
    let mut rows = statement
        .query([sql_number(plan.get())])
        .map_err(sql_error)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        result.push(ProposalRecord {
            entry: EntryId::new(row_number(row, 0).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            plan,
            base_revision: row_number(row, 1).map_err(sql_error)?,
            body: PlanText::new(row.get::<_, String>(2).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            state: row
                .get::<_, String>(3)
                .map_err(sql_error)?
                .parse::<ProposalState>()
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
        });
    }
    Ok(result)
}

fn feedback_for_plan(conn: &Connection, plan: PlanId) -> Result<Vec<FeedbackRecord>, BoardError> {
    let records = entries(
        conn,
        "SELECT e.id FROM entries e JOIN board_feedback f ON f.entry_id=e.id WHERE e.plan_id=?1 AND e.state IN ('open','triaged') ORDER BY e.seq,e.id",
        [sql_number(plan.get())],
    )?;
    records.into_iter().map(|entry| {
        let mut statement = conn.prepare("SELECT feedback_kind,version,build_id,cwd,steer_mode,recent_calls_json FROM board_feedback WHERE entry_id=?1").map_err(sql_error)?;
        let mut rows = statement.query([sql_number(entry.id.get())]).map_err(sql_error)?;
        let row = rows.next().map_err(sql_error)?.ok_or_else(|| invalid("board_unavailable", "feedback metadata is missing"))?;
        let state = match entry.state { Some(EntryState::Feedback(state)) => state, _ => return Err(invalid("board_unavailable", "feedback state is missing")) };
        Ok(FeedbackRecord {
            kind: row.get::<_, String>(0).map_err(sql_error)?.parse().map_err(BoardError::from)?, state,
            metadata: FeedbackMetadata {
                version: row.get(1).map_err(sql_error)?, build_id: row.get(2).map_err(sql_error)?,
                repo_key: entry.repo_key.clone(), cwd: row.get(3).map_err(sql_error)?, steer_mode: row.get(4).map_err(sql_error)?,
                recent_calls: decode_json(row.get(5).map_err(sql_error)?)?,
            }, entry,
        })
    }).collect()
}

fn linked_commits(
    conn: &Connection,
    plan: PlanId,
    start: i64,
    end: i64,
) -> Result<Vec<LinkedCommit>, BoardError> {
    let mut statement = conn.prepare("SELECT DISTINCT c.repo_key,c.oid,c.subject,c.committed_at,c.author,c.coauthors,c.files,c.insertions,c.deletions FROM commits c JOIN commit_plans p ON p.repo_key=c.repo_key AND p.oid=c.oid WHERE p.plan_id=?1 AND c.committed_at>=?2 AND c.committed_at<=?3 ORDER BY c.committed_at,c.repo_key,c.oid").map_err(sql_error)?;
    let mut rows = statement
        .query(params![sql_number(plan.get()), start, end])
        .map_err(sql_error)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let repo_key: RepoKey = row
            .get::<_, String>(0)
            .map_err(sql_error)?
            .parse()
            .map_err(BoardError::from)?;
        let oid: crate::identity::GitOid = row
            .get::<_, String>(1)
            .map_err(sql_error)?
            .parse()
            .map_err(BoardError::from)?;
        let mut links = conn.prepare("SELECT p.plan_id,t.task_ordinal FROM commit_plans p LEFT JOIN commit_tasks t ON t.repo_key=p.repo_key AND t.oid=p.oid AND t.plan_id=p.plan_id WHERE p.repo_key=?1 AND p.oid=?2 ORDER BY p.plan_id,t.task_ordinal").map_err(sql_error)?;
        let plans = links
            .query_map(params![repo_key.as_str(), oid.to_string()], |row| {
                Ok((row_number(row, 0)?, row.get::<_, Option<i64>>(1)?))
            })
            .map_err(sql_error)?
            .map(|link| {
                let (id, task_ordinal) = link.map_err(sql_error)?;
                Ok(CommitPlanLink {
                    plan_id: PlanId::new(id).map_err(BoardError::from)?,
                    task_ordinal: task_ordinal.map(sqlite_u64).transpose()?,
                })
            })
            .collect::<Result<Vec<_>, BoardError>>()?;
        result.push(LinkedCommit {
            repo_key,
            oid,
            subject: row.get(2).map_err(sql_error)?,
            committed_at: row.get(3).map_err(sql_error)?,
            author: row.get(4).map_err(sql_error)?,
            coauthors: decode_json(row.get(5).map_err(sql_error)?)?,
            files: row_number(row, 6).map_err(sql_error)?,
            insertions: row_number(row, 7).map_err(sql_error)?,
            deletions: row_number(row, 8).map_err(sql_error)?,
            plans,
        });
    }
    Ok(result)
}

pub(super) fn repositories(
    conn: &Connection,
    plan: Option<PlanId>,
) -> Result<BoardReply, BoardError> {
    if let Some(plan) = plan {
        require_plan(conn, plan)?;
    }
    let mut statement = conn.prepare("SELECT r.repo_key,r.origin_label,p.host,p.common_dir,p.scan_error FROM repos r JOIN repo_paths p ON p.repo_key=r.repo_key WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM plan_repos pr WHERE pr.repo_key=r.repo_key AND pr.plan_id=?1)) ORDER BY r.repo_key,p.host,p.common_dir").map_err(sql_error)?;
    let mut rows = statement
        .query([plan.map(|plan| sql_number(plan.get()))])
        .map_err(sql_error)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let repo_key: RepoKey = row
            .get::<_, String>(0)
            .map_err(sql_error)?
            .parse()
            .map_err(BoardError::from)?;
        let mut associations = conn.prepare("SELECT p.id,p.created_at FROM plans p JOIN plan_repos pr ON pr.plan_id=p.id WHERE pr.repo_key=?1 ORDER BY p.id").map_err(sql_error)?;
        let associations = associations
            .query_map([repo_key.as_str()], |row| {
                Ok((row_number(row, 0)?, row.get::<_, i64>(1)?))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?;
        let oldest_plan_at = associations
            .iter()
            .map(|(_, created)| *created)
            .min()
            .unwrap_or(0);
        let plans = associations
            .into_iter()
            .map(|(id, _)| PlanId::new(id).map_err(BoardError::from))
            .collect::<Result<Vec<_>, _>>()?;
        result.push(RepoScanTarget {
            registration: RepoRegistration {
                repo_key,
                origin_label: row.get(1).map_err(sql_error)?,
                host: row.get(2).map_err(sql_error)?,
                common_dir: PathBuf::from(row.get::<_, String>(3).map_err(sql_error)?),
                plan_id: plan,
            },
            oldest_plan_at,
            plans,
            scan_error: row.get(4).map_err(sql_error)?,
        });
    }
    Ok(BoardReply::new("local", BoardResult::Repositories(result)))
}

fn decode_json<T: serde::de::DeserializeOwned>(body: String) -> Result<T, BoardError> {
    serde_json::from_str(&body).map_err(|error| invalid("board_unavailable", error.to_string()))
}
