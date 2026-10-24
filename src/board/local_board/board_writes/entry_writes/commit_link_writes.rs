//! Idempotent commit links and their task activity evidence.

use super::*;
use crate::board::board_actor::{BoardActor, claim_vendor};
use crate::board::board_protocol::WorkerAddress;
use crate::board::commit_trailers::coauthors_prove_claim;
use crate::board::local_board::board_queries::worker_reads::worker_for_address;
use std::collections::{BTreeMap, BTreeSet};

mod manual_commit_link_writes;
pub(in crate::board::local_board) use manual_commit_link_writes::link_commit;

struct ClaimAttribution {
    entry: EntryId,
    actor_id: i64,
}

pub(in crate::board::local_board) fn link_commits(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    commits: &[LinkedCommit],
) -> Result<BoardReply, BoardError> {
    if commits.len() > 1 {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "commit scans write one commit per event sequence",
        ));
    }

    let mut result = CommitLinkResult::default();
    for commit in commits {
        let valid_links = validate_links(tx, commit, &mut result)?;
        if valid_links.is_empty() {
            continue;
        }

        insert_commit_row(tx, commit)?;
        for worker in &commit.worker_refs {
            if worker_for_address(tx, worker)?.is_none() && !result.unknown_workers.contains(worker)
            {
                result.unknown_workers.push(worker.clone());
            }
        }

        let mut first_entry = None;
        let mut event_plans = BTreeSet::new();
        let mut changed = false;
        for (plan, tasks) in valid_links {
            if !plan_link_restorable(tx, commit, plan, &tasks)? {
                continue;
            }
            let entry = ensure_scan_plan_link(tx, ctx, commit, plan)?;
            if entry.1 {
                changed = true;
                result.inserted += 1;
            }
            first_entry.get_or_insert(entry.0);
            event_plans.insert(plan);
            tx.execute(
                "INSERT OR IGNORE INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
                params![sql_number(plan.get()), commit.repo_key.as_str()],
            )
            .map_err(sql_error)?;

            for ordinal in tasks {
                let task = TaskId::new(plan, ordinal).map_err(BoardError::from)?;
                if task_link_suppressed(tx, &commit.repo_key, commit.oid, task)? {
                    continue;
                }
                let attribution = task_claim_attribution(tx, commit, task, ctx.now)?;
                let attribution = match attribution {
                    Some(candidate) if worker_refs_match_claim(tx, commit, task, &candidate)? => {
                        Some(candidate)
                    }
                    _ => None,
                };
                let (claim_entry, basis, claim_actor_id) =
                    attribution.as_ref().map_or((None, None, None), |claim| {
                        (
                            Some(sql_number(claim.entry.get())),
                            Some("claim_window"),
                            Some(claim.actor_id),
                        )
                    });
                let inserted = tx
                    .execute(
                        concat!(
                            "INSERT OR IGNORE INTO commit_tasks(",
                            "repo_key,oid,plan_id,task_ordinal,source,claim_entry,attribution_basis,claim_actor_id) ",
                            "VALUES(?1,?2,?3,?4,'scan',?5,?6,?7)"
                        ),
                        params![
                            commit.repo_key.as_str(),
                            commit.oid.as_str(),
                            sql_number(plan.get()),
                            sql_number(ordinal),
                            claim_entry,
                            basis,
                            claim_actor_id,
                        ],
                    )
                    .map_err(sql_error)?;
                if inserted > 0 {
                    task_claims::refresh_commit_claims(
                        tx,
                        plan,
                        ordinal,
                        &commit.coauthors,
                        commit.committed_at,
                        ctx.now,
                    )?;
                    changed = true;
                }
            }
        }

        if changed {
            let entry = first_entry.expect("valid commit links have a plan entry");
            let plan = (event_plans.len() == 1).then(|| *event_plans.first().expect("one plan"));
            let summary = linked_commit_summary(tx, commit)?;
            insert_event(
                tx,
                ctx,
                plan,
                EntryKind::Commit,
                &entry.to_string(),
                None,
                &summary,
            )?;
        }
    }
    result.unknown_plans.sort_unstable();
    result.unknown_plans.dedup();
    result.unknown_tasks.sort_unstable();
    result.unknown_tasks.dedup();
    Ok(BoardReply::new("local", BoardResult::CommitsLinked(result)))
}

/// Inserts immutable Git snapshots without rewriting an earlier manual receipt.
fn insert_commit_row(tx: &Transaction<'_>, commit: &LinkedCommit) -> Result<(), BoardError> {
    tx.execute(
        "INSERT OR IGNORE INTO repos(repo_key) VALUES(?1)",
        [commit.repo_key.as_str()],
    )
    .map_err(sql_error)?;
    let coauthors = serde_json::to_string(&commit.coauthors)
        .map_err(|error| invalid(BoardErrorCode::InvalidBody, error.to_string()))?;
    let worker_refs = serde_json::to_string(&commit.worker_refs)
        .map_err(|error| invalid(BoardErrorCode::InvalidBody, error.to_string()))?;
    tx.execute(
        concat!(
            "INSERT OR IGNORE INTO commits(repo_key,oid,subject,committed_at,author,coauthors,files,",
            "insertions,deletions,worker_refs_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"
        ),
        params![
            commit.repo_key.as_str(),
            commit.oid.as_str(),
            commit.subject,
            commit.committed_at,
            commit.author,
            coauthors,
            sql_number(commit.files),
            sql_number(commit.insertions),
            sql_number(commit.deletions),
            worker_refs,
        ],
    )
    .map_err(sql_error)?;
    tx.execute(
        concat!(
            "UPDATE commits SET worker_refs_json=?3 WHERE repo_key=?1 AND oid=?2 ",
            "AND worker_refs_json='[]' AND ?3<>'[]'"
        ),
        params![commit.repo_key.as_str(), commit.oid.as_str(), worker_refs],
    )
    .map_err(sql_error)?;
    Ok(())
}

/// Filters references by current board rows while retaining every valid task link.
fn validate_links(
    tx: &Transaction<'_>,
    commit: &LinkedCommit,
    result: &mut CommitLinkResult,
) -> Result<BTreeMap<PlanId, BTreeSet<u64>>, BoardError> {
    let mut requested = commit.plans.clone();
    requested.extend(commit.worker_refs.iter().map(|worker| {
        crate::board::board_protocol::CommitPlanLink {
            plan_id: worker.task.plan,
            task_ordinal: Some(worker.task.ordinal),
        }
    }));
    let mut valid = BTreeMap::<PlanId, BTreeSet<u64>>::new();
    for link in &requested {
        let plan_exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM plans WHERE id=?1)",
                [sql_number(link.plan_id.get())],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !plan_exists {
            result.unknown_plans.push(link.plan_id);
            continue;
        }
        let tasks = valid.entry(link.plan_id).or_default();
        if let Some(ordinal) = link.task_ordinal {
            let task_exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
                    params![sql_number(link.plan_id.get()), sql_number(ordinal)],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if task_exists {
                tasks.insert(ordinal);
            } else {
                result
                    .unknown_tasks
                    .push(TaskId::new(link.plan_id, ordinal).map_err(BoardError::from)?);
            }
        }
    }
    Ok(valid)
}

/// Plan-only trailers always link; a plan named by task trailers links only
/// while one of those tasks is not suppressed by an unlink receipt.
fn plan_link_restorable(
    tx: &Transaction<'_>,
    commit: &LinkedCommit,
    plan: PlanId,
    ordinals: &BTreeSet<u64>,
) -> Result<bool, BoardError> {
    if ordinals.is_empty() {
        return Ok(true);
    }
    for ordinal in ordinals {
        let task = TaskId::new(plan, *ordinal).map_err(BoardError::from)?;
        if !task_link_suppressed(tx, &commit.repo_key, commit.oid, task)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Creates the System-authored plan entry only when the plan link is new.
fn ensure_scan_plan_link(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    commit: &LinkedCommit,
    plan: PlanId,
) -> Result<(EntryId, bool), BoardError> {
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
    if let Some(entry) = existing {
        return Ok((
            EntryId::new(sqlite_u64(entry)?).map_err(BoardError::from)?,
            false,
        ));
    }
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
        concat!(
            "INSERT OR IGNORE INTO commit_plans(repo_key,oid,plan_id,entry_id,source) ",
            "VALUES(?1,?2,?3,?4,'scan')"
        ),
        params![
            commit.repo_key.as_str(),
            commit.oid.as_str(),
            sql_number(plan.get()),
            sql_number(entry.get())
        ],
    )
    .map_err(sql_error)?;
    let stored: i64 = tx
        .query_row(
            "SELECT entry_id FROM commit_plans WHERE repo_key=?1 AND oid=?2 AND plan_id=?3",
            params![
                commit.repo_key.as_str(),
                commit.oid.as_str(),
                sql_number(plan.get())
            ],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let stored = EntryId::new(sqlite_u64(stored)?).map_err(BoardError::from)?;
    Ok((stored, stored == entry))
}

/// A validated unlink receipt prevents scans from restoring an explicit removal.
fn task_link_suppressed(
    tx: &Transaction<'_>,
    repo: &RepoKey,
    oid: crate::identity::GitOid,
    task: TaskId,
) -> Result<bool, BoardError> {
    tx.query_row(
        concat!(
            "SELECT EXISTS(SELECT 1 FROM entries e ",
            "JOIN entry_refs task_ref ON task_ref.entry_id=e.id AND task_ref.target=?1 ",
            "JOIN entry_refs commit_ref ON commit_ref.entry_id=e.id AND commit_ref.target=?2 ",
            "JOIN events v ON v.seq=e.seq AND v.kind='unlinked' AND v.plan_id=e.plan_id ",
            "AND v.subject='E'||e.id AND v.actor_id=e.actor_id ",
            "WHERE e.kind='unlinked' AND e.repo_key=?3 AND e.plan_id=?4)"
        ),
        params![
            task.to_string(),
            oid.to_string(),
            repo.as_str(),
            sql_number(task.plan.get())
        ],
        |row| row.get(0),
    )
    .map_err(sql_error)
}

/// Chooses one non-boundary claim interval whose vendor appears in the snapshot.
fn task_claim_attribution(
    tx: &Transaction<'_>,
    commit: &LinkedCommit,
    task: TaskId,
    now: i64,
) -> Result<Option<ClaimAttribution>, BoardError> {
    if commit.committed_at > now {
        return Ok(None);
    }
    let mut statement = tx
        .prepare(concat!(
            "SELECT c.entry_id,c.actor_id,c.claimed_at,c.ended_at,",
            "a.user,a.host,a.harness,a.session,e.model FROM claims c ",
            "JOIN actors a ON a.id=c.actor_id JOIN entries e ON e.id=c.entry_id ",
            "WHERE c.plan_id=?1 AND c.task_ordinal=?2 ORDER BY c.claimed_at,c.id"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal)
        ])
        .map_err(sql_error)?;
    let mut temporal_matches = 0usize;
    let mut compatible = None;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let entry =
            EntryId::new(row_number(row, 0).map_err(sql_error)?).map_err(BoardError::from)?;
        let actor_id: i64 = row.get(1).map_err(sql_error)?;
        let start: i64 = row.get(2).map_err(sql_error)?;
        let ended_at: Option<i64> = row.get(3).map_err(sql_error)?;
        let end = ended_at.unwrap_or(now).min(now);
        if commit.committed_at == start || commit.committed_at == end {
            return Ok(None);
        }
        if start >= commit.committed_at || commit.committed_at >= end {
            continue;
        }
        temporal_matches += 1;
        if temporal_matches > 1 {
            return Ok(None);
        }
        let actor = actor_from_columns(row, 4)?;
        let model: Option<String> = row.get(8).map_err(sql_error)?;
        if coauthors_prove_claim(
            &commit.coauthors,
            claim_vendor(&actor.harness, model.as_deref()),
            &actor.harness,
        ) {
            compatible = Some(ClaimAttribution { entry, actor_id });
        }
    }
    Ok((temporal_matches == 1).then_some(compatible).flatten())
}

fn actor_from_columns(row: &rusqlite::Row<'_>, start: usize) -> Result<BoardActor, BoardError> {
    let user: String = row.get(start).map_err(sql_error)?;
    let host: String = row.get(start + 1).map_err(sql_error)?;
    let harness: String = row.get(start + 2).map_err(sql_error)?;
    let session: String = row.get(start + 3).map_err(sql_error)?;
    BoardActor::new(
        &user,
        &host,
        crate::request_context::HarnessLabel::parse(&harness).map_err(BoardError::from)?,
        &session,
    )
    .map_err(BoardError::from)
}

/// A known worker reference constrains inferred attribution to its original claim.
fn worker_refs_match_claim(
    tx: &Transaction<'_>,
    commit: &LinkedCommit,
    task: TaskId,
    claim: &ClaimAttribution,
) -> Result<bool, BoardError> {
    for address in commit
        .worker_refs
        .iter()
        .filter(|address| address.task == task)
    {
        if let Some(worker) = worker_for_address(tx, address)?
            && worker.claim_entry != claim.entry
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Emits a searchable receipt with immutable commit metadata and inferred claims.
fn linked_commit_summary(
    tx: &Transaction<'_>,
    commit: &LinkedCommit,
) -> Result<String, BoardError> {
    let (coauthors_json, linked_tasks): (String, Vec<(TaskId, Option<i64>)>) = {
        let coauthors: String = tx
            .query_row(
                "SELECT coauthors FROM commits WHERE repo_key=?1 AND oid=?2",
                params![commit.repo_key.as_str(), commit.oid.as_str()],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        let mut statement = tx
            .prepare(concat!(
                "SELECT plan_id,task_ordinal,claim_entry FROM commit_tasks ",
                "WHERE repo_key=?1 AND oid=?2 ORDER BY plan_id,task_ordinal"
            ))
            .map_err(sql_error)?;
        let rows = statement
            .query_map(
                params![commit.repo_key.as_str(), commit.oid.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get(2)?)),
            )
            .map_err(sql_error)?;
        let mut tasks = Vec::new();
        for row in rows {
            let (plan, ordinal, claim) = row.map_err(sql_error)?;
            let plan = PlanId::new(sqlite_u64(plan)?).map_err(BoardError::from)?;
            let ordinal = sqlite_u64(ordinal)?;
            tasks.push((TaskId::new(plan, ordinal).map_err(BoardError::from)?, claim));
        }
        (coauthors, tasks)
    };
    let coauthors: Vec<crate::board::board_protocol::CommitCoauthor> =
        serde_json::from_str(&coauthors_json)
            .map_err(|error| invalid(BoardErrorCode::BoardUnavailable, error.to_string()))?;
    let mut model_names = BTreeSet::new();
    model_names.extend(coauthors.iter().map(|author| author.model.as_str()));
    let task_text = linked_tasks
        .iter()
        .map(|(task, _)| task.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let model_text = model_names.into_iter().collect::<Vec<_>>().join(", ");
    let claims = linked_tasks
        .iter()
        .filter_map(|(_, claim)| {
            claim
                .and_then(|id| sqlite_u64(id).ok())
                .map(|id| format!("E{id}"))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!(
        "linked commit {} ({}) {} | tasks: [{}] | models: [{}] | claim evidence: [{}]",
        commit.oid,
        &commit.oid.as_str()[..8],
        commit.subject,
        task_text,
        model_text,
        claims,
    );
    Ok(bounded_summary(&text))
}

/// Conservatively fills claim links for existing rows after schema upgrades.
pub(in crate::board::local_board) fn backfill_commit_claim_links(
    tx: &Transaction<'_>,
    now: i64,
) -> Result<usize, BoardError> {
    let mut statement = tx
        .prepare(concat!(
            "SELECT c.repo_key,c.oid,c.subject,c.committed_at,c.author,c.coauthors,c.files,",
            "c.insertions,c.deletions,c.worker_refs_json,t.plan_id,t.task_ordinal ",
            "FROM commit_tasks t JOIN commits c ON c.repo_key=t.repo_key AND c.oid=t.oid ",
            "WHERE t.claim_entry IS NULL AND t.attribution_basis IS NULL ",
            "ORDER BY c.committed_at,c.repo_key,c.oid,t.plan_id,t.task_ordinal"
        ))
        .map_err(sql_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
            ))
        })
        .map_err(sql_error)?;
    let mut pending = Vec::new();
    for row in rows {
        let (
            repo,
            oid,
            subject,
            committed_at,
            author,
            coauthors,
            files,
            insertions,
            deletions,
            workers,
            plan,
            ordinal,
        ) = row.map_err(sql_error)?;
        let repo_key = RepoKey::parse(&repo).map_err(BoardError::from)?;
        let oid = oid
            .parse::<crate::identity::GitOid>()
            .map_err(BoardError::from)?;
        let coauthors: Vec<crate::board::board_protocol::CommitCoauthor> =
            serde_json::from_str(&coauthors)
                .map_err(|error| invalid(BoardErrorCode::BoardUnavailable, error.to_string()))?;
        let worker_refs: Vec<WorkerAddress> = serde_json::from_str(&workers)
            .map_err(|error| invalid(BoardErrorCode::BoardUnavailable, error.to_string()))?;
        let commit = LinkedCommit {
            repo_key,
            oid,
            subject,
            committed_at,
            author,
            coauthors,
            files: sqlite_u64(files)?,
            insertions: sqlite_u64(insertions)?,
            deletions: sqlite_u64(deletions)?,
            plans: Vec::new(),
            worker_refs,
            claim_links: Vec::new(),
        };
        let task = TaskId::new(
            PlanId::new(sqlite_u64(plan)?).map_err(BoardError::from)?,
            sqlite_u64(ordinal)?,
        )
        .map_err(BoardError::from)?;
        if let Some(claim) = task_claim_attribution(tx, &commit, task, now)?
            && worker_refs_match_claim(tx, &commit, task, &claim)?
        {
            pending.push((
                commit.repo_key,
                commit.oid,
                task,
                claim.entry,
                claim.actor_id,
            ));
        }
    }
    drop(statement);
    let mut updated = 0;
    for (repo, oid, task, claim, actor_id) in pending {
        updated += tx
            .execute(
                concat!(
                    "UPDATE commit_tasks SET claim_entry=?5,attribution_basis='claim_window',claim_actor_id=?6 ",
                    "WHERE repo_key=?1 AND oid=?2 AND plan_id=?3 AND task_ordinal=?4 ",
                    "AND claim_entry IS NULL AND attribution_basis IS NULL"
                ),
                params![
                    repo.as_str(),
                    oid.as_str(),
                    sql_number(task.plan.get()),
                    sql_number(task.ordinal),
                    sql_number(claim.get()),
                    actor_id,
                ],
            )
            .map_err(sql_error)?;
    }
    Ok(updated)
}

#[cfg(test)]
mod tests;
