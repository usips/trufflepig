//! Deadline-bound host reads of current task-linked commit identities.
use super::{BoardBackend, BoardHost, board_writer::check_deadline};
use crate::{
    board::{board_ids::RepoKey, local_board::LocalBoard},
    daemon::deadline::QueryDeadline,
    identity::GitOid,
};
use anyhow::Result;
use std::{collections::BTreeSet, time::Duration};

impl BoardHost {
    pub(super) fn linked_commit_oids_by(
        &self,
        repo_key: &RepoKey,
        oids: &[GitOid],
        deadline: QueryDeadline,
    ) -> Result<BTreeSet<GitOid>> {
        check_deadline(deadline)?;
        if oids.is_empty() {
            return Ok(BTreeSet::new());
        }
        let config = self.config()?;
        let reader =
            LocalBoard::open_read_with_timeout(&config, deadline.cap(Duration::from_secs(5)))?;
        check_deadline(deadline)?;
        reader
            .linked_commit_oids(repo_key, oids)
            .map_err(Into::into)
    }
}
