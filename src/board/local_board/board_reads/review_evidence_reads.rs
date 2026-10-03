//! Plan review windows and durable proposal, feedback, and commit evidence.

use super::*;

pub(in crate::board::local_board) fn review(
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
    let mut statement = conn.prepare("SELECT p.entry_id,p.base_revision,t.body,p.state,p.decision_entry,p.result_revision,p.base_revision<plan.head_revision FROM proposals p JOIN texts t ON t.hash=p.text_hash JOIN plans plan ON plan.id=p.plan_id WHERE p.plan_id=?1 AND p.state='open' ORDER BY p.entry_id").map_err(sql_error)?;
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
            stale_base: row.get(6).map_err(sql_error)?,
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
