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
        via: None,
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
    let mut registration = registration.clone();
    let prior: Option<(String, String)> = tx.query_row(
        "SELECT repo_key,root_commits_json FROM repo_paths WHERE host=?1 AND common_dir=?2 ORDER BY rowid LIMIT 1",
        params![registration.host, registration.common_dir.to_string_lossy().as_ref()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(sql_error)?;
    if let Some((key, roots)) = prior {
        let roots: Vec<crate::identity::GitOid> = serde_json::from_str(&roots)
            .map_err(|error| invalid("board_unavailable", error.to_string()))?;
        let initial_key: RepoKey = key.parse().map_err(BoardError::from)?;
        if let Some(configured) = &registration.origin_override {
            if configured != &initial_key {
                return Err(invalid(
                    "invalid_options",
                    format!(
                        "origin override {configured} conflicts with registered repository identity {initial_key}"
                    ),
                ));
            }
        }
        registration.repo_key = initial_key;
        if !roots.is_empty() {
            registration.root_commits = roots;
        }
    }
    tx.execute(
        "INSERT OR IGNORE INTO plan_repos(plan_id,repo_key) SELECT pr.plan_id,?1 FROM plan_repos pr JOIN repo_paths p ON p.repo_key=pr.repo_key WHERE p.host=?2 AND p.common_dir=?3",
        params![registration.repo_key.as_str(), registration.host, registration.common_dir.to_string_lossy().as_ref()],
    ).map_err(sql_error)?;
    tx.execute(
        "DELETE FROM repo_paths WHERE host=?1 AND common_dir=?2 AND repo_key<>?3",
        params![
            registration.host,
            registration.common_dir.to_string_lossy().as_ref(),
            registration.repo_key.as_str()
        ],
    )
    .map_err(sql_error)?;
    let roots = serde_json::to_string(&registration.root_commits)
        .map_err(|error| invalid("invalid_body", error.to_string()))?;
    tx.execute("INSERT INTO repos(repo_key,origin_label) VALUES(?1,?2) ON CONFLICT(repo_key) DO UPDATE SET origin_label=COALESCE(excluded.origin_label,repos.origin_label)",params![registration.repo_key.as_str(),registration.origin_label]).map_err(sql_error)?;
    tx.execute(
        "INSERT INTO repo_paths(repo_key,host,common_dir,root_commits_json,registration_error) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(repo_key,host,common_dir) DO UPDATE SET root_commits_json=excluded.root_commits_json,registration_error=excluded.registration_error",
        params![
            registration.repo_key.as_str(),
            registration.host,
            registration.common_dir.to_string_lossy().as_ref(),
            roots,
            registration.registration_error
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

pub(super) fn forget_repo_path(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    repo_key: &RepoKey,
    host: &str,
    common_dir: &Path,
) -> Result<BoardReply, BoardError> {
    if host != ctx.actor.host {
        return Err(invalid(
            "invalid_actor",
            "repository path host differs from actor host",
        ));
    }
    tx.execute(
        "DELETE FROM repo_paths WHERE repo_key=?1 AND host=?2 AND common_dir=?3",
        params![
            repo_key.as_str(),
            host,
            common_dir.to_string_lossy().as_ref()
        ],
    )
    .map_err(sql_error)?;
    Ok(BoardReply::new("local", BoardResult::RepoPathForgotten))
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
        let mut valid_links =
            std::collections::BTreeMap::<PlanId, std::collections::BTreeSet<u64>>::new();
        for link in &commit.plans {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM plans WHERE id=?1)",
                    [sql_number(link.plan_id.get())],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if !exists {
                result.unknown_plans.push(link.plan_id);
                continue;
            }
            let tasks = valid_links.entry(link.plan_id).or_default();
            if let Some(ordinal) = link.task_ordinal {
                let exists: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
                        params![sql_number(link.plan_id.get()), sql_number(ordinal)],
                        |row| row.get(0),
                    )
                    .map_err(sql_error)?;
                if exists {
                    tasks.insert(ordinal);
                } else {
                    result.unknown_tasks.push(
                        crate::board::board_ids::TaskId::new(link.plan_id, ordinal)
                            .map_err(BoardError::from)?,
                    );
                }
            }
        }
        if valid_links.is_empty() {
            continue;
        }
        tx.execute("INSERT OR IGNORE INTO commits(repo_key,oid,subject,committed_at,author,coauthors,files,insertions,deletions) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![commit.repo_key.as_str(),commit.oid.to_string(),commit.subject,commit.committed_at,commit.author,coauthors,sql_number(commit.files),sql_number(commit.insertions),sql_number(commit.deletions)]).map_err(sql_error)?;
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
            via: None,
        };
        for (plan, tasks) in valid_links {
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT entry_id FROM commit_plans WHERE repo_key=?1 AND oid=?2 AND plan_id=?3",
                    params![
                        commit.repo_key.as_str(),
                        commit.oid.as_str(),
                        sql_number(plan.get())
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_error)?;
            if existing.is_none() {
                let body = bounded_summary(&format!("{} {}", commit.oid, commit.subject));
                let entry = insert_entry(
                    tx,
                    &commit_ctx,
                    &EntryDraft {
                        plan_id: Some(plan),
                        kind: EntryKind::Commit,
                        body,
                        to_whom: None,
                        supersedes: None,
                        repo_key: Some(commit.repo_key.clone()),
                        state: None,
                    },
                )?;
                tx.execute(
                    "INSERT INTO commit_plans(repo_key,oid,plan_id,entry_id) VALUES(?1,?2,?3,?4)",
                    params![
                        commit.repo_key.as_str(),
                        commit.oid.as_str(),
                        sql_number(plan.get()),
                        sql_number(entry.get())
                    ],
                )
                .map_err(sql_error)?;
                if event_plan.is_some_and(|prior| prior != plan) {
                    mixed_plans = true;
                }
                event_plan.get_or_insert(plan);
                first_entry.get_or_insert(entry);
                result.inserted += 1;
            }
            tx.execute(
                "INSERT OR IGNORE INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
                params![sql_number(plan.get()), commit.repo_key.as_str()],
            )
            .map_err(sql_error)?;
            for ordinal in tasks {
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO commit_tasks(repo_key,oid,plan_id,task_ordinal) VALUES(?1,?2,?3,?4)",
                    params![commit.repo_key.as_str(),commit.oid.as_str(),sql_number(plan.get()),sql_number(ordinal)],
                ).map_err(sql_error)?;
                if inserted > 0 {
                    task_claims::refresh_commit_claims(
                        tx,
                        plan,
                        ordinal,
                        &commit.coauthors,
                        commit.committed_at,
                        ctx.now,
                    )?;
                }
            }
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
    result.unknown_plans.sort_unstable();
    result.unknown_plans.dedup();
    result.unknown_tasks.sort_unstable();
    result.unknown_tasks.dedup();
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

#[cfg(test)]
mod tests;
