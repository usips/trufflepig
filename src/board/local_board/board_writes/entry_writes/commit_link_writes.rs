//! Idempotent commit links and their task activity evidence.

use super::*;

pub(in crate::board::local_board) fn link_commits(
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
        tx.execute(
            concat!(
                "INSERT OR IGNORE INTO commits(repo_key,oid,subject,committed_at,author,coauthors,files,",
                "insertions,deletions) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)"
            ),
            params![
                commit.repo_key.as_str(),
                commit.oid.to_string(),
                commit.subject,
                commit.committed_at,
                commit.author,
                coauthors,
                sql_number(commit.files),
                sql_number(commit.insertions),
                sql_number(commit.deletions)
            ],
        )
        .map_err(sql_error)?;
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
            EntryKind::Commit,
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

/// Manual links name the linker and record `manual` provenance; a relink
/// replays the original receipt without new rows, entries, or events.
pub(in crate::board::local_board) fn link_commit(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    commit: &LinkedCommit,
) -> Result<BoardReply, BoardError> {
    let plan = task.plan;
    require_link_authority(tx, &ctx.actor, plan)?;
    let task_exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
            params![sql_number(plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if !task_exists {
        return Err(invalid("invalid_reference", format!("unknown task {task}")));
    }
    tx.execute(
        "INSERT OR IGNORE INTO repos(repo_key) VALUES(?1)",
        [commit.repo_key.as_str()],
    )
    .map_err(sql_error)?;
    let coauthors =
        serde_json::to_string(&commit.coauthors).map_err(|e| invalid("invalid_body", e.to_string()))?;
    tx.execute(
        concat!(
            "INSERT OR IGNORE INTO commits(repo_key,oid,subject,committed_at,author,coauthors,files,",
            "insertions,deletions) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)"
        ),
        params![
            commit.repo_key.as_str(),
            commit.oid.to_string(),
            commit.subject,
            commit.committed_at,
            commit.author,
            coauthors,
            sql_number(commit.files),
            sql_number(commit.insertions),
            sql_number(commit.deletions)
        ],
    )
    .map_err(sql_error)?;
    let existing: Option<(i64, i64)> = tx
        .query_row(
            concat!(
                "SELECT p.entry_id,e.seq FROM commit_plans p JOIN entries e ON e.id=p.entry_id ",
                "WHERE p.repo_key=?1 AND p.oid=?2 AND p.plan_id=?3"
            ),
            params![
                commit.repo_key.as_str(),
                commit.oid.as_str(),
                sql_number(plan.get())
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let (entry, seq, deduplicated) = match existing {
        Some((entry, seq)) => (
            EntryId::new(sqlite_u64(entry)?).map_err(BoardError::from)?,
            EventSeq::new(sqlite_u64(seq)?),
            true,
        ),
        None => {
            let body = bounded_summary(&format!("{} {}", commit.oid, commit.subject));
            let entry = insert_entry(
                tx,
                ctx,
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
                "INSERT INTO commit_plans(repo_key,oid,plan_id,entry_id,source) VALUES(?1,?2,?3,?4,'manual')",
                params![
                    commit.repo_key.as_str(),
                    commit.oid.as_str(),
                    sql_number(plan.get()),
                    sql_number(entry.get())
                ],
            )
            .map_err(sql_error)?;
            (entry, ctx.seq, false)
        }
    };
    tx.execute(
        "INSERT OR IGNORE INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
        params![sql_number(plan.get()), commit.repo_key.as_str()],
    )
    .map_err(sql_error)?;
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO commit_tasks(repo_key,oid,plan_id,task_ordinal,source) VALUES(?1,?2,?3,?4,'manual')",
        params![
            commit.repo_key.as_str(),
            commit.oid.as_str(),
            sql_number(plan.get()),
            sql_number(task.ordinal)
        ],
    ).map_err(sql_error)?;
    if inserted > 0 {
        task_claims::refresh_commit_claims(
            tx,
            plan,
            task.ordinal,
            &commit.coauthors,
            commit.committed_at,
            ctx.now,
        )?;
    }
    if !deduplicated {
        insert_event(
            tx,
            ctx,
            Some(plan),
            EntryKind::Commit,
            &entry.to_string(),
            None,
            &format!("linked {} to {task} by hand", commit.oid),
        )?;
    }
    Ok(BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry,
            seq,
            plan: Some(plan),
            revision: None,
            task: Some(task),
            deduplicated,
        }),
    ))
}

/// The plan's steward harness, the plan owner's user, or any human may repair links.
fn require_link_authority(
    tx: &Transaction<'_>,
    actor: &BoardActor,
    plan: PlanId,
) -> Result<(), BoardError> {
    let (owner, steward): (String, Option<String>) = tx
        .query_row(
            "SELECT owner_user,steward FROM plans WHERE id=?1",
            [sql_number(plan.get())],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown plan {plan}")))?;
    if actor.harness.is_human()
        || actor.harness.is_cli()
        || actor.user == owner
        || steward.as_deref() == Some(actor.harness.as_str())
    {
        return Ok(());
    }
    Err(invalid(
        "invalid_actor",
        format!("linking a commit to {plan} requires its steward, its owner, or a human"),
    ))
}
