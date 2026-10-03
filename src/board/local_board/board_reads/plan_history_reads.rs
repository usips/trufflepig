//! Immutable plan revisions and the current plan view.

use super::*;

pub(super) fn plans(conn: &Connection) -> Result<Vec<PlanRecord>, BoardError> {
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

pub(super) fn revision(conn: &Connection, id: PlanRevision) -> Result<RevisionRecord, BoardError> {
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
