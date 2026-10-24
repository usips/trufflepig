//! Mutation operation application inside the serialized dispatch transaction.

use super::*;

pub(super) fn apply_mutation(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    request: &BoardRequest,
) -> Result<BoardReply, BoardError> {
    match &request.op {
        BoardOp::TaskCreate { plan, .. } | BoardOp::CarveClaim { plan, .. } => {
            board_writes::plan_lifecycle::require_open(tx, *plan)?;
        }
        BoardOp::TaskMove { task, .. }
        | BoardOp::ClaimTask { task, .. }
        | BoardOp::WorkerAdd { task, .. } => {
            board_writes::plan_lifecycle::require_open(tx, task.plan)?;
            board_writes::task_transfers::require_current(tx, *task)?;
        }
        BoardOp::Propose { base, .. } | BoardOp::Edit { base, .. } => {
            board_writes::plan_lifecycle::require_open(tx, base.plan)?;
        }
        _ => {}
    }
    match &request.op {
        BoardOp::Hello {
            model,
            effort,
            worker,
        } => board_writes::entry_writes::hello(tx, ctx, model, effort.as_deref(), worker.as_ref()),
        BoardOp::Inbox {
            after,
            limit,
            scope,
        } => board_feed::inbox(tx, ctx, *after, *limit, scope),
        BoardOp::AcknowledgeInbox { rendered_through } => {
            board_feed::acknowledge(tx, ctx, *rendered_through)
        }
        BoardOp::New {
            title,
            body,
            repo_key,
        } => board_writes::plan_writes::new_plan(tx, ctx, title, body, repo_key.as_ref()),
        BoardOp::Post {
            target,
            kind,
            body,
            to,
            supersedes,
            progress,
            verdict,
        } => board_writes::entry_writes::post(
            tx,
            ctx,
            target,
            *kind,
            body,
            to.as_ref(),
            *supersedes,
            progress.as_ref(),
            verdict.as_ref(),
            request.trusted_head.as_ref(),
        ),
        BoardOp::Grant { .. } => write_grant(tx, ctx, &request.op),
        BoardOp::Revoke { .. } => write_revoke(tx, ctx, &request.op),
        BoardOp::Bye => board_writes::task_claims::bye(tx, ctx),
        BoardOp::WorkerAdd {
            task,
            handle,
            role,
            model,
            effort,
            scope,
            files,
        } => board_writes::task_claims::add_worker(
            tx,
            ctx,
            *task,
            handle,
            *role,
            model,
            effort.as_deref(),
            scope,
            files,
        ),
        BoardOp::WorkerEnd {
            worker,
            state,
            note,
        } => board_writes::task_claims::end_worker(tx, ctx, worker, *state, note.as_ref()),
        BoardOp::TaskCreate {
            plan,
            title,
            to,
            section,
        } => board_writes::task_writes::create_task(
            tx,
            ctx,
            *plan,
            title,
            to.as_ref(),
            section.as_deref(),
        ),
        BoardOp::TaskMove { task, column, to } => {
            board_writes::task_writes::move_task(tx, ctx, *task, *column, to.as_ref())
        }
        BoardOp::TaskTransfer {
            task,
            destination,
            note,
        } => board_writes::task_transfers::transfer_task(tx, ctx, *task, *destination, note),
        BoardOp::PlanClose { plan, note } => {
            board_writes::plan_lifecycle::close_plan(tx, ctx, *plan, note)
        }
        BoardOp::PlanReopen { plan, note } => {
            board_writes::plan_lifecycle::reopen_plan(tx, ctx, *plan, note)
        }
        BoardOp::ClaimTask {
            task,
            scope,
            resume,
            delegate,
        } => board_writes::task_claims::claim_task(
            tx,
            ctx,
            *task,
            scope.as_ref(),
            *resume,
            delegate.as_ref(),
        ),
        BoardOp::CarveClaim {
            plan,
            title,
            scope,
            section,
        } => {
            board_writes::task_claims::carve_claim(tx, ctx, *plan, title, scope, section.as_deref())
        }
        BoardOp::Propose {
            base,
            body,
            summary,
            supersedes,
        } => board_writes::plan_writes::propose(tx, ctx, *base, body, summary, *supersedes),
        BoardOp::Edit {
            base,
            body,
            summary,
        } => board_writes::plan_writes::edit(tx, ctx, *base, body, summary),
        BoardOp::Accept { proposal, note } => {
            board_writes::plan_writes::accept(tx, ctx, *proposal, note.as_ref())
        }
        BoardOp::Reject { proposal, reason } => {
            board_writes::plan_writes::reject(tx, ctx, *proposal, reason)
        }
        BoardOp::Feedback { .. } => {
            board_writes::feedback_entries::write_feedback(tx, ctx, &request.op)
        }
        BoardOp::FeedbackTriage { .. } | BoardOp::FeedbackClose { .. } => {
            board_writes::feedback_entries::close_feedback(tx, ctx, &request.op)
        }
        BoardOp::RegisterRepo { registration } => {
            board_writes::entry_writes::register_repo(tx, ctx, registration)
        }
        BoardOp::Show { .. }
        | BoardOp::Receipt { .. }
        | BoardOp::Pending { .. }
        | BoardOp::Search { .. }
        | BoardOp::Review { .. }
        | BoardOp::FeedbackList { .. }
        | BoardOp::Repositories { .. }
        | BoardOp::Projects
        | BoardOp::Overview { .. }
        | BoardOp::Attention { .. }
        | BoardOp::Feed { .. }
        | BoardOp::History { .. }
        | BoardOp::Entries { .. }
        | BoardOp::Tasks { .. }
        | BoardOp::DoneTasks { .. }
        | BoardOp::Claims { .. } => unreachable!("read operations use query-only dispatch"),
        BoardOp::RecordScan {
            repo_key,
            host,
            common_dir,
            error,
        } => board_writes::entry_writes::record_scan(
            tx,
            repo_key,
            host,
            common_dir,
            error.as_deref(),
        ),
        BoardOp::ForgetRepoPath {
            repo_key,
            host,
            common_dir,
        } => board_writes::entry_writes::forget_repo_path(tx, ctx, repo_key, host, common_dir),
        BoardOp::LinkCommits { commits } => {
            board_writes::entry_writes::link_commits(tx, ctx, commits)
        }
        BoardOp::LinkCommit {
            task, resolution, ..
        } => {
            let commit = resolution.as_deref().ok_or_else(|| {
                invalid(
                    BoardErrorCode::InvalidOptions,
                    "commit links resolve through the board host",
                )
            })?;
            board_writes::entry_writes::link_commit(tx, ctx, *task, commit)
        }
        BoardOp::UnlinkCommit { oid, task } => {
            board_writes::entry_writes::unlink_commit(tx, ctx, *oid, *task)
        }
    }
}
