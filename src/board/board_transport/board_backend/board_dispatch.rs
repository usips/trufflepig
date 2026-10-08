//! Request dispatch, repository registration and edge rendering.
mod host_run;
#[cfg(test)]
mod tests;
use crate::board::board_protocol::{BoardOp, ReadScope, RepoRegistration};

/// Bare CLI reads use the caller's repository; explicit scopes remain concrete.
fn scope_read_repo_key(op: &mut BoardOp, registration: Option<&RepoRegistration>, all: bool) {
    if all {
        return;
    }
    let scope = match op {
        BoardOp::Inbox { scope, .. }
        | BoardOp::Attention { scope, .. }
        | BoardOp::Claims {
            scope,
            own_stale: true,
            ..
        }
        | BoardOp::Overview { scope, .. }
        | BoardOp::DoneTasks { scope, .. } => scope,
        _ => return,
    };
    if matches!(scope, ReadScope::All | ReadScope::Repo(_)) {
        *scope = registration.map_or(ReadScope::All, |repo| {
            ReadScope::Repo(repo.repo_key.clone())
        });
    }
}
