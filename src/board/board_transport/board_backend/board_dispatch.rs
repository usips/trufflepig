//! Request dispatch, repository registration and edge rendering.
mod host_run;
#[cfg(test)]
mod tests;
use crate::board::board_protocol::{BoardOp, RepoRegistration};

/// Scoped reads default to the caller's canonical repository; `all` stays global.
fn scope_read_repo_key(op: &mut BoardOp, registration: Option<&RepoRegistration>) {
    let repo_key = match op {
        BoardOp::Inbox {
            repo_key,
            all: false,
            ..
        }
        | BoardOp::Attention {
            repo_key,
            all: false,
            ..
        }
        | BoardOp::Claims {
            repo_key,
            all: false,
            ..
        }
        | BoardOp::Overview {
            repo_key,
            all: false,
            ..
        } => repo_key,
        _ => return,
    };
    *repo_key = registration.map(|registration| registration.repo_key.clone());
}
