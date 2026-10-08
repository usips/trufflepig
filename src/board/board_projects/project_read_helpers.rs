//! Project selection and counted wire replies at the serving-host boundary.
use super::ResolvedProject;
use crate::board::{
    board_config::BoardConfig,
    board_ids::EventSeq,
    board_protocol::{
        BoardError, BoardErrorCode, BoardOp, BoardReply, BoardResult, ProjectMemberRecord,
        ProjectRecord, ReadScope,
    },
    local_board::LocalBoard,
};
use crate::daemon::deadline::QueryDeadline;
use std::time::Duration;

pub(crate) fn select_scope(
    op: &mut BoardOp,
    selector: &str,
    projects: &[ResolvedProject],
) -> Result<(), BoardError> {
    validate_scope_selection(op)?;
    let selected = select_project(selector, projects)?;
    *op.read_scope_mut().expect("scope was validated") =
        ReadScope::Keys(selected.project.repo_keys.clone());
    Ok(())
}

pub(crate) fn validate_scope_selection(op: &mut BoardOp) -> Result<(), BoardError> {
    let scope = op
        .read_scope_mut()
        .ok_or_else(|| invalid("project requires a scoped read"))?;
    if !scope.is_all() {
        return Err(invalid(
            "project cannot be combined with an explicit repository scope",
        ));
    }
    Ok(())
}

pub(crate) fn select_project<'a>(
    selector: &str,
    projects: &'a [ResolvedProject],
) -> Result<&'a ResolvedProject, BoardError> {
    let selected = if let Some(project) = projects.iter().find(|row| row.project.id == selector) {
        project
    } else {
        let mut matches = projects.iter().filter(|row| row.raw_name == selector);
        let first = matches
            .next()
            .ok_or_else(|| invalid(format!("unknown project {selector}")))?;
        if let Some(second) = matches.next() {
            let mut ids = Vec::with_capacity(2 + matches.size_hint().0);
            ids.push(first.project.id.as_str());
            ids.push(second.project.id.as_str());
            ids.extend(matches.map(|row| row.project.id.as_str()));
            ids.sort_unstable();
            return Err(invalid(format!(
                "project {selector} matches {}",
                ids.join(", ")
            )));
        }
        first
    };
    Ok(selected)
}

pub(crate) fn project_reply(
    config: &BoardConfig,
    projects: Vec<ResolvedProject>,
    deadline: QueryDeadline,
) -> Result<BoardReply, BoardError> {
    check_deadline(deadline)?;
    let present = match std::fs::symlink_metadata(&config.db_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(BoardError::from(anyhow::Error::new(error))),
    };
    let (snapshot, counts) = if present {
        let mut reader =
            LocalBoard::open_read_with_timeout(config, deadline.cap(Duration::from_secs(5)))?;
        let sets: Vec<_> = projects.iter().map(|row| &row.project.repo_keys).collect();
        reader.repo_set_plan_counts(&sets, deadline)?
    } else {
        (EventSeq::new(0), vec![0; projects.len()])
    };
    check_deadline(deadline)?;
    let records = projects
        .into_iter()
        .zip(counts)
        .map(|(resolved, plan_count)| {
            let project = resolved.project;
            ProjectRecord {
                id: project.id,
                name: project.name,
                repo_keys: project.repo_keys,
                unavailable: project
                    .unavailable
                    .into_iter()
                    .map(|member| ProjectMemberRecord {
                        name: member.name,
                        root: crate::store::encode_path(&member.root),
                        available: member.available,
                    })
                    .collect(),
                plan_count,
            }
        })
        .collect();
    let mut reply = BoardReply::new(
        format!("local:{}", config.db_path.display()),
        BoardResult::Projects(records),
    );
    reply.snapshot_seq = Some(snapshot);
    Ok(reply)
}

fn invalid(message: impl Into<String>) -> BoardError {
    BoardError::new(BoardErrorCode::InvalidOptions, message)
}

fn check_deadline(deadline: QueryDeadline) -> Result<(), BoardError> {
    if deadline.expired() {
        return Err(BoardError::new(
            BoardErrorCode::DaemonBusy,
            "project read timed out",
        ));
    }
    Ok(())
}
