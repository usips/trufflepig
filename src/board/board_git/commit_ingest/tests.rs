mod coauthor_records;
mod commit_records;
mod linked_warning_cache;
mod repository_paths;
mod scan_cache;
mod scan_resilience;
mod warning_subjects;

use super::*;
use crate::board::board_ids::EventSeq;
use crate::board::board_protocol::{BoardError, BoardReply, CommitLinkResult};
use crate::board::repo_identity::tests::GitFixture;
use crate::identity::GitOid;
use std::collections::BTreeSet;

#[derive(Default)]
struct TestBackend {
    linked: Vec<LinkedCommit>,
    requests: usize,
    fail_next_link: bool,
    unknown_plans: Vec<PlanId>,
    scan_errors: Vec<Option<String>>,
    forgotten: usize,
    fail_forget: bool,
    manual_oids: BTreeSet<(RepoKey, GitOid)>,
}
impl BoardBackend for TestBackend {
    fn handle(&mut self, request: &BoardRequest) -> std::result::Result<BoardReply, BoardError> {
        let result = match &request.op {
            BoardOp::LinkCommits { commits } => {
                self.requests += 1;
                if self.fail_next_link {
                    self.fail_next_link = false;
                    return Err(
                        anyhow::anyhow!("board_unavailable: injected linking failure").into(),
                    );
                }
                self.linked.extend(commits.iter().cloned());
                BoardResult::CommitsLinked(CommitLinkResult {
                    inserted: commits.len() as u64,
                    unknown_plans: self.unknown_plans.clone(),
                    unknown_tasks: Vec::new(),
                })
            }
            BoardOp::ForgetRepoPath { .. } => {
                if self.fail_forget {
                    return Err(anyhow::anyhow!("database is locked: cleanup fixture").into());
                }
                self.forgotten += 1;
                BoardResult::RepoPathForgotten
            }
            BoardOp::RecordScan { error, .. } => {
                self.scan_errors.push(error.clone());
                BoardResult::ScanRecorded
            }
            _ => panic!("unexpected test operation"),
        };
        Ok(BoardReply::new("test", result))
    }
    fn import_feedback(
        &mut self,
        request: &BoardRequest,
    ) -> std::result::Result<BoardReply, BoardError> {
        self.handle(request)
    }
    fn max_seq(&self) -> std::result::Result<EventSeq, BoardError> {
        Ok(EventSeq::new(0))
    }
    fn linked_commit_oids(
        &self,
        repo_key: &RepoKey,
        oids: &[GitOid],
    ) -> std::result::Result<BTreeSet<GitOid>, BoardError> {
        Ok(oids
            .iter()
            .copied()
            .filter(|oid| {
                self.manual_oids.contains(&(repo_key.clone(), *oid))
                    || self.linked.iter().any(|commit| {
                        &commit.repo_key == repo_key
                            && commit.oid == *oid
                            && commit.plans.iter().any(|link| link.task_ordinal.is_some())
                    })
            })
            .collect())
    }
}

fn actor() -> BoardActor {
    BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("codex").unwrap(),
        "git-test",
    )
    .unwrap()
}
fn target(fixture: &GitFixture) -> RepoScanTarget {
    RepoScanTarget {
        registration: fixture.registration(),
        oldest_plan_at: 1_700_000_000,
        plans: vec![PlanId::new(7).unwrap()],
        scan_error: None,
    }
}
fn linked_message(subject: &str) -> String {
    format!(
        "{subject}\n\nPlan: P7\nPlan-Task: P7.3\nCo-authored-by: Model claim <noreply@openai.com>"
    )
}
