//! Manual link authority and relink dedupe decisions.

use super::*;

impl LocalBoard {
    /// Checks manual link authority without creating actor or event state.
    pub(crate) fn require_commit_link_authority(
        &mut self,
        actor: &BoardActor,
        plan: PlanId,
        now: i64,
    ) -> Result<AuthoritySource, BoardError> {
        let tx = self.conn.transaction().map_err(sql_error)?;
        require_link_authority(&tx, actor, plan, now)
    }
}

/// Resolves the actor's current link authority for a plan.
pub(in crate::local_board) fn require_link_authority(
    tx: &Transaction<'_>,
    actor: &BoardActor,
    plan: PlanId,
    now: i64,
) -> Result<AuthoritySource, BoardError> {
    super::super::super::board_authority::authority_for(
        tx,
        actor,
        Some(plan),
        AuthorityRole::Link,
        now,
    )?
    .ok_or_else(|| {
        invalid(BoardErrorCode::InvalidActor,
            format!(
                "changing commit links for {plan} requires link authority; ask its owner to grant link"
            ),
        )
    })
}

/// The shared plan-level commit entry and its original entry sequence.
pub(in crate::local_board) struct PlanCommitEntry {
    pub(in crate::local_board) entry: EntryId,
    pub(in crate::local_board) seq: EventSeq,
}

/// Existing scan links do not have task-specific manual event receipts.
pub(in crate::local_board) enum TaskLinkReceipt {
    Manual(EventSeq),
    ManualWithoutEvent,
    Scan,
}

/// The entry associated with an existing plan-level commit link, if any.
pub(in crate::local_board) fn plan_commit_entry(
    tx: &Transaction<'_>,
    commit: &LinkedCommit,
    plan: PlanId,
) -> Result<Option<PlanCommitEntry>, BoardError> {
    tx.query_row(
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
    .map_err(sql_error)?
    .map(|(entry, seq): (i64, i64)| {
        Ok(PlanCommitEntry {
            entry: EntryId::new(sqlite_u64(entry)?).map_err(BoardError::from)?,
            seq: EventSeq::new(sqlite_u64(seq)?),
        })
    })
    .transpose()
}

/// The durable receipt for an existing commit-to-task association.
pub(in crate::local_board) fn task_link_receipt(
    tx: &Transaction<'_>,
    repo: &RepoKey,
    oid: tp_core::identity::GitOid,
    task: TaskId,
) -> Result<Option<TaskLinkReceipt>, BoardError> {
    let row: Option<(String, Option<i64>)> = tx
        .query_row(
            concat!(
                "SELECT source,link_seq FROM commit_tasks ",
                "WHERE repo_key=?1 AND oid=?2 AND plan_id=?3 AND task_ordinal=?4"
            ),
            params![
                repo.as_str(),
                oid.as_str(),
                sql_number(task.plan.get()),
                sql_number(task.ordinal)
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(source, seq)| match (source.as_str(), seq) {
        ("manual", Some(seq)) => Ok(TaskLinkReceipt::Manual(EventSeq::new(sqlite_u64(seq)?))),
        ("manual", None) => Ok(TaskLinkReceipt::ManualWithoutEvent),
        ("scan", _) => Ok(TaskLinkReceipt::Scan),
        _ => Err(invalid(
            BoardErrorCode::InvalidState,
            format!("commit link for {task} has unknown source {source:?}"),
        )),
    })
    .transpose()
}

/// Creates the commit entry and `manual` plan link for a first-time link.
pub(in crate::local_board) fn record_plan_link(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    commit: &LinkedCommit,
    plan: PlanId,
) -> Result<PlanCommitEntry, BoardError> {
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
    Ok(PlanCommitEntry {
        entry,
        seq: ctx.seq,
    })
}
