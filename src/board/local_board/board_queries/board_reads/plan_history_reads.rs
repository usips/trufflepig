//! Immutable plan revisions and the current plan view.

use super::*;

pub(super) fn plan(conn: &Connection, id: PlanId) -> Result<PlanRecord, BoardError> {
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

pub(in crate::board::local_board) fn plan_row(row: &Row<'_>) -> Result<PlanRecord, BoardError> {
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

pub(super) fn revision(conn: &Connection, id: PlanRevision) -> Result<RevisionRecord, BoardError> {
    let mut statement = conn.prepare(
        concat!(
            "SELECT t.body,r.source,r.entry_id,a.user,a.host,a.harness,a.session,r.seq,e.created_at ",
            "FROM revisions r JOIN texts t ON t.hash=r.text_hash JOIN actors a ON a.id=r.actor_id ",
            "JOIN entries e ON e.id=r.entry_id WHERE r.plan_id=?1 AND r.number=?2"
        ),
    ).map_err(sql_error)?;
    let mut rows = statement
        .query(params![sql_number(id.plan.get()), sql_number(id.revision)])
        .map_err(sql_error)?;
    let row = rows
        .next()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown revision {id}")))?;
    let source: String = row.get(1).map_err(sql_error)?;
    let source = RevisionSource::parse(&source)
        .map_err(|error| invalid("board_unavailable", error.to_string()))?;
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

pub(super) fn plan_view(
    conn: &Connection,
    ctx: &WriteContext,
    id: PlanId,
) -> Result<PlanView, BoardError> {
    let plan = plan(conn, id)?;
    let revision = revision(
        conn,
        PlanRevision::new(id, plan.head_revision).map_err(BoardError::from)?,
    )?;
    let through = crate::board::local_board::max_seq(conn)?;
    let tasks = crate::board::local_board::board_queries::collection_nested::task_window(
        conn,
        id,
        None,
        None,
        Some(through),
        200,
    )?;
    let tasks_omitted = tasks.omitted;
    let tasks_next_after = tasks.next_after;
    let task_ceiling = tasks.ceiling;
    let tasks = tasks.tasks;
    let claims = crate::board::local_board::board_queries::collection_nested::claim_window(
        conn,
        ctx,
        Some(id),
        false,
        None,
        true,
        None,
        Some(through),
        200,
    )?;
    let claims_omitted = claims.omitted;
    let claims_next_after = claims.next_after;
    let claims = claims.claims.into_iter().map(|view| view.claim).collect();
    let entries = entries(
        conn,
        &format!(
            concat!(
                "SELECT e.id FROM entries e WHERE e.plan_id=?1 AND (e.id IN (SELECT id FROM entries ",
                "WHERE plan_id=?1 ORDER BY seq DESC,id DESC LIMIT ?2) ",
                "OR EXISTS(SELECT 1 FROM proposals p WHERE p.entry_id=e.id AND p.state='open') ",
                "OR (e.kind='feedback' AND e.state IN ('open','triaged')) OR ({OPEN_QUESTION})) ",
                "ORDER BY e.seq DESC,e.id DESC LIMIT 200"
            ),
            OPEN_QUESTION = OPEN_QUESTION
        ),
        params![sql_number(id.get()), RECENT_ENTRIES],
    )?;
    let entries_total: i64 = conn.query_row(
        &format!(
            concat!(
                "SELECT count(*) FROM entries e WHERE e.plan_id=?1 AND (e.id IN (SELECT id FROM entries ",
                "WHERE plan_id=?1 ORDER BY seq DESC,id DESC LIMIT ?2) ",
                "OR EXISTS(SELECT 1 FROM proposals p WHERE p.entry_id=e.id AND p.state='open') ",
                "OR (e.kind='feedback' AND e.state IN ('open','triaged')) OR ({OPEN_QUESTION}))"
            ),
            OPEN_QUESTION = OPEN_QUESTION
        ),
        params![sql_number(id.get()), RECENT_ENTRIES], |row| row.get(0),
    ).map_err(sql_error)?;
    let entries_omitted = usize::try_from(entries_total)
        .map_err(|_| invalid("board_unavailable", "invalid entry count"))?
        .saturating_sub(entries.len());
    let (commits, commits_omitted) = linked_commits_bounded(conn, id, i64::MIN, i64::MAX, 200)?;
    let mut sections_without_tasks = Vec::new();
    for section in uncovered_sections(revision.body.as_str(), &[]) {
        let covered: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND section=?2)",
                params![sql_number(id.get()), section],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !covered {
            sections_without_tasks.push(section);
        }
    }
    let can_edit = crate::board::local_board::can_accept(conn, &ctx.actor, id)?;
    let entries_next_before = (entries_omitted > 0)
        .then(|| {
            entries.last().map(|entry| EntryCursor {
                seq: entry.seq,
                entry: entry.id,
            })
        })
        .flatten();
    let entries_next_after: Option<EntryCursor> = None;
    Ok(PlanView {
        plan,
        revision,
        tasks,
        task_ceiling,
        claims,
        entries,
        commits,
        tasks_omitted,
        claims_omitted,
        entries_omitted,
        commits_omitted,
        tasks_next_after,
        claims_next_after,
        entries_next_after,
        entries_next_before,
        through,
        can_edit,
        server_now: ctx.now,
        claim_ttl_secs: ctx.claim_ttl_secs.max(0) as u64,
        sections_without_tasks,
    })
}

fn uncovered_sections(body: &str, tasks: &[TaskRecord]) -> Vec<String> {
    let headings = crate::board::board_markup::headings(body);
    let mut sections = Vec::with_capacity(headings.len());
    for heading in headings {
        if heading.title.is_empty()
            || tasks
                .iter()
                .any(|task| task.section.as_deref() == Some(heading.title.as_str()))
            || sections.iter().any(|section| section == &heading.title)
        {
            continue;
        }
        sections.push(heading.title);
    }
    sections
}
