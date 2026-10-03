//! Commit metadata reads preserve every plan/task link and bound plan projections.
use super::{BoardError, invalid, row_number, sql_error, sql_number, sqlite_u64};
use crate::board::{
    board_ids::{EntryId, PlanId, RepoKey},
    board_protocol::{CommitPlanLink, LinkedCommit},
};
use rusqlite::{Connection, params};

const COMMIT_COLUMNS: &str = "c.repo_key,c.oid,c.subject,c.committed_at,c.author,c.coauthors,c.files,c.insertions,c.deletions";
const PLAN_WINDOW: &str = "EXISTS(SELECT 1 FROM commit_plans p WHERE p.repo_key=c.repo_key AND p.oid=c.oid AND p.plan_id=?1) AND c.committed_at>=?2 AND c.committed_at<=?3";

pub(super) fn linked_commits_bounded(
    conn: &Connection,
    plan: PlanId,
    start: i64,
    end: i64,
    limit: usize,
) -> Result<(Vec<LinkedCommit>, usize), BoardError> {
    let bound = i64::try_from(
        limit
            .checked_add(1)
            .ok_or_else(|| invalid("invalid_options", "commit limit overflow"))?,
    )
    .map_err(|_| invalid("invalid_options", "commit limit exceeds SQLite range"))?;
    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM commits c WHERE {PLAN_WINDOW}"),
            params![sql_number(plan.get()), start, end],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let total = usize::try_from(total)
        .map_err(|_| invalid("board_unavailable", "commit count exceeds platform range"))?;
    let commits = read_plan_window(conn, plan, start, end, bound, limit)?;
    let omitted = total.saturating_sub(commits.len());
    Ok((commits, omitted))
}

pub(super) fn linked_commits_all(
    conn: &Connection,
    plan: PlanId,
    start: i64,
    end: i64,
) -> Result<Vec<LinkedCommit>, BoardError> {
    read_plan_window(conn, plan, start, end, -1, usize::MAX)
}

fn read_plan_window(
    conn: &Connection,
    plan: PlanId,
    start: i64,
    end: i64,
    bound: i64,
    limit: usize,
) -> Result<Vec<LinkedCommit>, BoardError> {
    let mut statement = conn.prepare(&format!("SELECT {COMMIT_COLUMNS} FROM commits c WHERE {PLAN_WINDOW} ORDER BY c.committed_at,c.repo_key,c.oid LIMIT ?4")).map_err(sql_error)?;
    let mut rows = statement
        .query(params![sql_number(plan.get()), start, end, bound])
        .map_err(sql_error)?;
    let mut result = Vec::with_capacity(limit.min(200));
    while let Some(row) = rows.next().map_err(sql_error)? {
        if result.len() == limit {
            break;
        }
        result.push(decode_commit(conn, row)?);
    }
    Ok(result)
}

pub(super) fn linked_commit_for_entry(
    conn: &Connection,
    entry: EntryId,
) -> Result<Option<LinkedCommit>, BoardError> {
    let mut statement = conn.prepare(&format!("SELECT {COMMIT_COLUMNS} FROM commits c JOIN commit_plans p ON p.repo_key=c.repo_key AND p.oid=c.oid WHERE p.entry_id=?1 LIMIT 1")).map_err(sql_error)?;
    let mut rows = statement
        .query([sql_number(entry.get())])
        .map_err(sql_error)?;
    rows.next()
        .map_err(sql_error)?
        .map(|row| decode_commit(conn, row))
        .transpose()
}

fn decode_commit(conn: &Connection, row: &rusqlite::Row<'_>) -> Result<LinkedCommit, BoardError> {
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
        .query_map(params![repo_key.as_str(), oid.as_str()], |row| {
            Ok((row_number(row, 0)?, row.get::<_, Option<i64>>(1)?))
        })
        .map_err(sql_error)?
        .map(|link| {
            let (id, ordinal) = link.map_err(sql_error)?;
            Ok(CommitPlanLink {
                plan_id: PlanId::new(id).map_err(BoardError::from)?,
                task_ordinal: ordinal.map(sqlite_u64).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, BoardError>>()?;
    let coauthors: String = row.get(5).map_err(sql_error)?;
    Ok(LinkedCommit {
        repo_key,
        oid,
        subject: row.get(2).map_err(sql_error)?,
        committed_at: row.get(3).map_err(sql_error)?,
        author: row.get(4).map_err(sql_error)?,
        coauthors: serde_json::from_str(&coauthors)
            .map_err(|error| invalid("board_unavailable", error.to_string()))?,
        files: row_number(row, 6).map_err(sql_error)?,
        insertions: row_number(row, 7).map_err(sql_error)?,
        deletions: row_number(row, 8).map_err(sql_error)?,
        plans,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_commits_count_omissions_and_entry_lookup_retains_every_link() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE commits(repo_key TEXT,oid TEXT,subject TEXT,committed_at INTEGER,author TEXT,coauthors TEXT,files INTEGER,insertions INTEGER,deletions INTEGER); CREATE TABLE commit_plans(repo_key TEXT,oid TEXT,plan_id INTEGER,entry_id INTEGER); CREATE TABLE commit_tasks(repo_key TEXT,oid TEXT,plan_id INTEGER,task_ordinal INTEGER);").unwrap();
        let key = "a".repeat(40);
        for number in 1..=3 {
            let oid = format!("{number:040x}");
            conn.execute(
                "INSERT INTO commits VALUES(?1,?2,?3,?4,'Fixture','[]',1,2,3)",
                params![key, oid, format!("commit{number}"), number],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO commit_plans VALUES(?1,?2,1,?3)",
                params![key, oid, number],
            )
            .unwrap();
        }
        let first = format!("{:040x}", 1);
        conn.execute(
            "INSERT INTO commit_plans VALUES(?1,?2,2,99)",
            params![key, first],
        )
        .unwrap();
        for task in [1, 2] {
            conn.execute(
                "INSERT INTO commit_tasks VALUES(?1,?2,1,?3)",
                params![key, first, task],
            )
            .unwrap();
        }
        let (commits, omitted) =
            linked_commits_bounded(&conn, PlanId::new(1).unwrap(), 0, 10, 2).unwrap();
        assert_eq!(
            commits
                .iter()
                .map(|commit| commit.subject.as_str())
                .collect::<Vec<_>>(),
            ["commit1", "commit2"]
        );
        assert_eq!(omitted, 1);
        let full = linked_commit_for_entry(&conn, EntryId::new(1).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(full.plans.len(), 3);
        assert_eq!(full.plans[0].task_ordinal, Some(1));
        assert_eq!(full.plans[1].task_ordinal, Some(2));
        assert_eq!(full.plans[2].plan_id, PlanId::new(2).unwrap());
        assert_eq!(full.insertions, 2);
        assert!(
            linked_commit_for_entry(&conn, EntryId::new(42).unwrap())
                .unwrap()
                .is_none()
        );
    }
}
