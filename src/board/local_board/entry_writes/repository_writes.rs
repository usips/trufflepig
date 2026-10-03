//! Repository identity registration and checkout scan state.

use super::*;

pub(in crate::board::local_board) fn register_repo(
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

pub(in crate::board::local_board) fn forget_repo_path(
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

pub(in crate::board::local_board) fn record_scan(
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
