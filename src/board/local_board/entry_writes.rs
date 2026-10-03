//! Session snapshots, append-only posts, and idempotent repository evidence.

use rusqlite::{Transaction, params};

use super::*;
use crate::board::board_actor::BoardRecipient;
use crate::board::board_vocabulary::EntryText;

pub(super) fn hello(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    model: &str,
    effort: Option<&str>,
) -> Result<BoardReply, BoardError> {
    tx.execute(
        "UPDATE agent_sessions SET model=?2,effort=?3,last_seen=?4 WHERE actor_id=?1",
        params![ctx.actor_id, model, effort, ctx.now],
    )
    .map_err(sql_error)?;
    let snapshot = WriteContext {
        actor_id: ctx.actor_id,
        actor: ctx.actor.clone(),
        model: Some(model.to_owned()),
        effort: effort.map(str::to_owned),
        now: ctx.now,
        seq: ctx.seq,
        claim_ttl_secs: ctx.claim_ttl_secs,
        dedupe_key: ctx.dedupe_key.clone(),
    };
    let body = effort.map_or_else(|| model.to_owned(), |effort| format!("{model}/{effort}"));
    let entry = insert_entry(
        tx,
        &snapshot,
        &EntryDraft {
            plan_id: None,
            kind: EntryKind::Hello,
            body: body.clone(),
            to_whom: None,
            supersedes: None,
            repo_key: None,
            state: None,
            dedupe_key: None,
        },
    )?;
    insert_event(
        tx,
        &snapshot,
        None,
        "hello",
        &entry.to_string(),
        None,
        &body,
    )?;
    let (cursor, first_seen, last_seen): (Option<i64>, i64, i64) = tx
        .query_row(
            "SELECT cursor_seq,first_seen,last_seen FROM agent_sessions WHERE actor_id=?1",
            [ctx.actor_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sql_error)?;
    Ok(BoardReply::new(
        "local",
        BoardResult::Session(SessionRecord {
            actor: ctx.actor.clone(),
            model: Some(model.to_owned()),
            effort: effort.map(str::to_owned),
            cursor: EventSeq::new(sqlite_u64(cursor.unwrap_or(0))?),
            first_seen,
            last_seen,
        }),
    ))
}

pub(super) fn post(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    target: &BoardRef,
    kind: EntryKind,
    body: &EntryText,
    to: Option<&BoardRecipient>,
    supersedes: Option<EntryId>,
) -> Result<BoardReply, BoardError> {
    let plan = target
        .plan_id()
        .ok_or_else(|| invalid("invalid_reference", "post requires a plan or task"))?;
    require_plan(tx, plan)?;
    if let BoardRef::Task(task) = target {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
                params![sql_number(plan.get()), sql_number(task.ordinal)],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if !exists {
            return Err(invalid("invalid_reference", format!("unknown task {task}")));
        }
    }
    if let Some(previous) = supersedes {
        let prior = read_entry(tx, previous)?;
        if prior.plan != Some(plan) {
            return Err(invalid(
                "invalid_reference",
                "superseded entry belongs to a different plan",
            ));
        }
    }
    let entry = insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: Some(plan),
            kind,
            body: body.as_str().to_owned(),
            to_whom: to.map(|to| to.as_str().to_owned()),
            supersedes,
            repo_key: None,
            state: None,
            dedupe_key: None,
        },
    )?;
    if matches!(target, BoardRef::Task(_)) {
        tx.execute(
            "INSERT OR IGNORE INTO entry_refs(entry_id,target) VALUES(?1,?2)",
            params![sql_number(entry.get()), target.to_string()],
        )
        .map_err(sql_error)?;
    }
    insert_event(
        tx,
        ctx,
        Some(plan),
        kind.as_str(),
        &entry.to_string(),
        to.map(BoardRecipient::as_str),
        body.as_str(),
    )?;
    Ok(BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry,
            seq: ctx.seq,
            plan: Some(plan),
            revision: None,
            task: if let BoardRef::Task(task) = target {
                Some(*task)
            } else {
                None
            },
            deduplicated: false,
        }),
    ))
}

pub(super) fn register_repo(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    registration: &RepoRegistration,
) -> Result<BoardReply, BoardError> {
    if registration.host != ctx.actor.host {
        return Err(invalid(
            "invalid_actor",
            "repository path host differs from actor host",
        ));
    }
    if let Some(plan) = registration.plan_id {
        require_plan(tx, plan)?;
    }
    tx.execute("INSERT INTO repos(repo_key,origin_label) VALUES(?1,?2) ON CONFLICT(repo_key) DO UPDATE SET origin_label=COALESCE(excluded.origin_label,repos.origin_label)",params![registration.repo_key.as_str(),registration.origin_label]).map_err(sql_error)?;
    tx.execute(
        "INSERT OR IGNORE INTO repo_paths(repo_key,host,common_dir) VALUES(?1,?2,?3)",
        params![
            registration.repo_key.as_str(),
            registration.host,
            registration.common_dir.to_string_lossy().as_ref()
        ],
    )
    .map_err(sql_error)?;
    if let Some(plan) = registration.plan_id {
        tx.execute(
            "INSERT OR IGNORE INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
            params![sql_number(plan.get()), registration.repo_key.as_str()],
        )
        .map_err(sql_error)?;
    }
    Ok(BoardReply::new(
        "local",
        BoardResult::Registered(registration.clone()),
    ))
}

pub(super) fn record_scan(
    tx: &Transaction<'_>,
    repo_key: &RepoKey,
    host: &str,
    common_dir: &Path,
    error: Option<&str>,
) -> Result<BoardReply, BoardError> {
    let updated = tx
        .execute(
            "UPDATE repo_paths SET scan_error=?4 WHERE repo_key=?1 AND host=?2 AND common_dir=?3",
            params![
                repo_key.as_str(),
                host,
                common_dir.to_string_lossy().as_ref(),
                error
            ],
        )
        .map_err(sql_error)?;
    if updated == 0 {
        return Err(invalid(
            "invalid_reference",
            "scan target is not registered",
        ));
    }
    Ok(BoardReply::new("local", BoardResult::ScanRecorded))
}

pub(super) fn link_commits(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    commits: &[LinkedCommit],
) -> Result<BoardReply, BoardError> {
    let mut result = CommitLinkResult::default();
    let mut first_entry = None;
    let mut event_plan = None;
    let mut mixed_plans = false;
    for commit in commits {
        tx.execute(
            "INSERT OR IGNORE INTO repos(repo_key) VALUES(?1)",
            [commit.repo_key.as_str()],
        )
        .map_err(sql_error)?;
        let coauthors = serde_json::to_string(&commit.coauthors)
            .map_err(|e| invalid("invalid_body", e.to_string()))?;
        let stats = serde_json::to_string(&commit.file_stats)
            .map_err(|e| invalid("invalid_body", e.to_string()))?;
        let mut valid_links = Vec::with_capacity(commit.plans.len());
        for link in &commit.plans {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM plans WHERE id=?1)",
                    [sql_number(link.plan_id.get())],
                    |r| r.get(0),
                )
                .map_err(sql_error)?;
            if !exists {
                if !result.unknown_plans.contains(&link.plan_id) {
                    result.unknown_plans.push(link.plan_id);
                }
                continue;
            }
            if let Some(ordinal) = link.task_ordinal {
                let exists: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
                        params![sql_number(link.plan_id.get()), sql_number(ordinal)],
                        |r| r.get(0),
                    )
                    .map_err(sql_error)?;
                if !exists {
                    return Err(invalid(
                        "invalid_reference",
                        format!("unknown task {}.{ordinal}", link.plan_id),
                    ));
                }
            }
            let exists:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM commit_plans WHERE repo_key=?1 AND oid=?2 AND plan_id=?3)",params![commit.repo_key.as_str(),commit.oid.to_string(),sql_number(link.plan_id.get())],|r|r.get(0)).map_err(sql_error)?;
            if !exists
                && !valid_links
                    .iter()
                    .any(|prior: &CommitPlanLink| prior.plan_id == link.plan_id)
            {
                valid_links.push(link.clone());
            }
        }
        if valid_links.is_empty() {
            continue;
        }
        tx.execute("INSERT OR IGNORE INTO commits(repo_key,oid,subject,committed_at,author,coauthors,files,file_stats,insertions,deletions) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![commit.repo_key.as_str(),commit.oid.to_string(),commit.subject,commit.committed_at,commit.author,coauthors,sql_number(commit.files),stats,sql_number(commit.insertions),sql_number(commit.deletions)]).map_err(sql_error)?;
        let harness = commit.coauthors.first().map_or_else(
            || HarnessLabel::parse("human").map_err(BoardError::from),
            |author| Ok(author.harness.clone()),
        )?;
        let actor = BoardActor::new(
            &ctx.actor.user,
            &ctx.actor.host,
            harness,
            format!("git-{}", commit.oid),
        )
        .map_err(BoardError::from)?;
        let actor_id = ensure_actor(tx, &actor, ctx.now)?;
        let commit_ctx = WriteContext {
            actor_id,
            actor,
            model: commit.coauthors.first().map(|a| a.model.clone()),
            effort: None,
            now: ctx.now,
            seq: ctx.seq,
            claim_ttl_secs: ctx.claim_ttl_secs,
            dedupe_key: ctx.dedupe_key.clone(),
        };
        for link in valid_links {
            let body = bounded_summary(&format!("{} {}", commit.oid, commit.subject));
            let entry = insert_entry(
                tx,
                &commit_ctx,
                &EntryDraft {
                    plan_id: Some(link.plan_id),
                    kind: EntryKind::Commit,
                    body,
                    to_whom: None,
                    supersedes: None,
                    repo_key: Some(commit.repo_key.clone()),
                    state: None,
                    dedupe_key: None,
                },
            )?;
            tx.execute("INSERT INTO commit_plans(repo_key,oid,plan_id,task_ordinal,entry_id) VALUES(?1,?2,?3,?4,?5)",params![commit.repo_key.as_str(),commit.oid.to_string(),sql_number(link.plan_id.get()),link.task_ordinal.map(sql_number),sql_number(entry.get())]).map_err(sql_error)?;
            tx.execute(
                "INSERT OR IGNORE INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
                params![sql_number(link.plan_id.get()), commit.repo_key.as_str()],
            )
            .map_err(sql_error)?;
            if let Some(ordinal) = link.task_ordinal {
                task_claims::refresh_commit_claims(
                    tx,
                    link.plan_id,
                    ordinal,
                    &commit.coauthors,
                    commit.committed_at,
                    ctx.now,
                )?;
            }
            if let Some(plan) = event_plan {
                if plan != link.plan_id {
                    mixed_plans = true;
                }
            } else {
                event_plan = Some(link.plan_id);
            }
            first_entry.get_or_insert(entry);
            result.inserted += 1;
        }
    }
    if let Some(entry) = first_entry {
        let summary = format!("linked {} commit plan references", result.inserted);
        insert_event(
            tx,
            ctx,
            if mixed_plans { None } else { event_plan },
            "commit",
            &entry.to_string(),
            None,
            &summary,
        )?;
    }
    result.unknown_plans.sort_by_key(|plan| plan.get());
    Ok(BoardReply::new("local", BoardResult::CommitsLinked(result)))
}

fn bounded_summary(text: &str) -> String {
    let mut end = text
        .len()
        .min(crate::board::board_vocabulary::ENTRY_TEXT_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
