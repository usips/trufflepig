//! Registered checkout paths and associated plan boundaries.

use super::*;
use crate::board::local_board::LocalBoard;
use rusqlite::OptionalExtension;

impl LocalBoard {
    /// Reads durable plan associations without requiring registered host paths.
    pub(crate) fn plan_repo_keys(&self, plan: PlanId) -> Result<Vec<RepoKey>, BoardError> {
        let conn = self.reader.as_ref().unwrap_or(&self.conn);
        require_plan(conn, plan)?;
        crate::board::local_board::board_queries::read_scope_sql::plan_repo_keys(conn, plan)
    }

    /// Durable host/path identity; paths use the same lossy encoding as registration writes.
    pub fn repo_key_at(
        &self,
        host: &str,
        common_dir: &std::path::Path,
    ) -> Result<Option<RepoKey>, BoardError> {
        let key: Option<String> = self.reader.as_ref().unwrap_or(&self.conn)
            .query_row(
                "SELECT repo_key FROM repo_paths WHERE host=?1 AND common_dir=?2 ORDER BY rowid LIMIT 1",
                params![host, common_dir.to_string_lossy().as_ref()],
                |row| row.get(0),
            ).optional().map_err(sql_error)?;
        key.map(|key| key.parse().map_err(BoardError::from))
            .transpose()
    }
}

pub(in crate::board::local_board) fn repositories(
    conn: &Connection,
    plan: Option<PlanId>,
) -> Result<BoardReply, BoardError> {
    if let Some(plan) = plan {
        require_plan(conn, plan)?;
    }
    let mut statement = conn
        .prepare(concat!(
            "SELECT r.repo_key,r.origin_label,p.host,p.common_dir,p.scan_error,p.root_commits_json,",
            "p.registration_error FROM repos r JOIN repo_paths p ON p.repo_key=r.repo_key ",
            "WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM plan_repos pr WHERE pr.repo_key=r.repo_key AND pr.plan_id=?1)) ",
            "ORDER BY r.repo_key,p.host,p.common_dir"
        ))
        .map_err(sql_error)?;
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
        let mut associations = conn
            .prepare(concat!(
                "SELECT p.id,p.created_at FROM plans p JOIN plan_repos pr ON pr.plan_id=p.id ",
                "WHERE pr.repo_key=?1 ORDER BY p.id"
            ))
            .map_err(sql_error)?;
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
                origin_label: row
                    .get::<_, Option<String>>(1)
                    .map_err(sql_error)?
                    .as_deref()
                    .map(crate::board::repo_identity::normalize_origin_label)
                    .transpose()
                    .map_err(|error| invalid("board_unavailable", error.to_string()))?,
                host: row.get(2).map_err(sql_error)?,
                common_dir: PathBuf::from(row.get::<_, String>(3).map_err(sql_error)?),
                plan_id: plan,
                root_commits: decode_json(row.get::<_, String>(5).map_err(sql_error)?)?,
                registration_error: row.get(6).map_err(sql_error)?,
                origin_override: None,
            },
            oldest_plan_at,
            plans,
            scan_error: row.get(4).map_err(sql_error)?,
        });
    }
    Ok(BoardReply::new("local", BoardResult::Repositories(result)))
}
