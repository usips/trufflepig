//! Local Git scans; only accepted complete scans advance edge-local digests.
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::{PlanId, RepoKey, TaskId};
use crate::board::board_protocol::{
    BoardOp, BoardRequest, BoardResult, LinkedCommit, RepoRegistration, RepoScanTarget,
};
use crate::board::commit_trailers::attributed_to;
use anyhow::{Result, ensure};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

mod commit_scanner;
#[cfg(test)]
mod tests;
use commit_scanner::{collect_tips, scan_log};

const COMMIT_LIMIT: usize = 2000;
const MISSING_REPOSITORY_GRACE: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Default)]
pub struct IngestReport {
    pub scanned: u64,
    pub skipped: u64,
    pub inserted: u64,
    pub unknown_plans: Vec<PlanId>,
    pub unknown_tasks: Vec<TaskId>,
    pub completed: Vec<RepoRegistration>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct UnlinkedScan {
    pub commits: Vec<LinkedCommit>,
    #[cfg(test)]
    pub complete: bool,
    pub scan_error: Option<String>,
}

#[derive(Default)]
pub struct RepoIngestor {
    completed: HashMap<(RepoKey, PathBuf), CompletedScan>,
    missing: HashMap<(RepoKey, PathBuf), Instant>,
}

struct CompletedScan {
    stamp: ScanStamp,
    unknown_plans: Vec<PlanId>,
    unknown_tasks: Vec<TaskId>,
    warnings: Vec<String>,
}

#[derive(PartialEq)]
struct ScanStamp {
    digest: [u8; 32],
    since: i64,
    plans: Vec<PlanId>,
}

impl RepoIngestor {
    pub fn ingest(
        &mut self,
        backend: &mut dyn BoardBackend,
        actor: &BoardActor,
        targets: &[RepoScanTarget],
        timeout: Duration,
    ) -> Result<IngestReport> {
        let deadline = Instant::now() + timeout;
        let mut report = IngestReport::default();
        for target in targets {
            if target.registration.host != actor.host {
                report.skipped += 1;
                continue;
            }
            let key = (
                target.registration.repo_key.clone(),
                target.registration.common_dir.clone(),
            );
            if std::fs::metadata(&target.registration.common_dir)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                let first_missing = self.missing.entry(key.clone()).or_insert_with(Instant::now);
                if first_missing.elapsed() >= MISSING_REPOSITORY_GRACE {
                    match backend.handle(&BoardRequest::new(
                        actor.clone(),
                        BoardOp::ForgetRepoPath {
                            repo_key: target.registration.repo_key.clone(),
                            host: actor.host.clone(),
                            common_dir: target.registration.common_dir.clone(),
                        },
                    )) {
                        Ok(reply) if matches!(reply.result, BoardResult::RepoPathForgotten) => {
                            self.completed.remove(&key);
                            self.missing.remove(&key);
                        }
                        Ok(_) => report
                            .errors
                            .push("board_scan: unexpected repository cleanup reply".into()),
                        Err(error) => report.errors.push(format!(
                            "{}: repository cleanup failed: {error}",
                            target.registration.common_dir.display()
                        )),
                    }
                } else if !target.plans.is_empty() {
                    report.errors.push(format!(
                        "{}: repository common directory is missing; cleanup waits for grace period",
                        target.registration.common_dir.display()
                    ));
                }
                report.skipped += 1;
                continue;
            }
            self.missing.remove(&key);
            if target.plans.is_empty() {
                report.skipped += 1;
                continue;
            }
            let result = self.ingest_one(backend, actor, target, deadline, &mut report);
            if let Err(error) = result {
                let message = format!("{}: {error:#}", target.registration.common_dir.display());
                report.errors.push(message.clone());
                if let Err(error) = record_scan(backend, actor, &target.registration, Some(message))
                {
                    report.errors.push(format!("record_scan: {error:#}"));
                }
            }
        }
        report.unknown_plans.sort_unstable();
        report.unknown_plans.dedup();
        report.unknown_tasks.sort_unstable();
        report.unknown_tasks.dedup();
        let mut seen = std::collections::HashSet::with_capacity(report.errors.len());
        report.errors.retain(|error| seen.insert(error.clone()));
        Ok(report)
    }

    fn ingest_one(
        &mut self,
        backend: &mut dyn BoardBackend,
        actor: &BoardActor,
        target: &RepoScanTarget,
        deadline: Instant,
        report: &mut IngestReport,
    ) -> Result<()> {
        let registration = &target.registration;
        let tips = collect_tips(registration, deadline)?;
        let mut plans = target.plans.clone();
        plans.sort_unstable();
        plans.dedup();
        let since = target.oldest_plan_at.saturating_sub(86_400);
        let stamp = ScanStamp {
            digest: tips.digest,
            since,
            plans,
        };
        let key = (
            registration.repo_key.clone(),
            registration.common_dir.clone(),
        );
        if let Some(completed) = self
            .completed
            .get(&key)
            .filter(|completed| completed.stamp == stamp && target.scan_error.is_none())
        {
            report
                .unknown_plans
                .extend_from_slice(&completed.unknown_plans);
            report
                .unknown_tasks
                .extend_from_slice(&completed.unknown_tasks);
            report.errors.extend_from_slice(&completed.warnings);
            report.skipped += 1;
            report.completed.push(registration.clone());
            return Ok(());
        }
        report.scanned += 1;
        let scan = scan_log(registration, &tips, since, true, deadline)?;
        let commits = scan
            .records
            .into_iter()
            .filter(|record| !record.commit.plans.is_empty())
            .map(|record| record.commit)
            .collect();
        let reply = backend.handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::LinkCommits { commits },
        ))?;
        let BoardResult::CommitsLinked(result) = reply.result else {
            anyhow::bail!("board_scan: unexpected commit-link reply");
        };
        report.inserted += result.inserted;
        report
            .unknown_plans
            .extend_from_slice(&result.unknown_plans);
        report
            .unknown_tasks
            .extend_from_slice(&result.unknown_tasks);
        report.errors.extend_from_slice(&scan.warnings);
        ensure!(
            scan.complete,
            "board_scan: commit scan exceeds {COMMIT_LIMIT} records"
        );
        ensure!(
            collect_tips(registration, deadline)?.digest == tips.digest,
            "board_scan: Git tips changed during scan"
        );
        record_scan(backend, actor, registration, None)?;
        report.completed.push(registration.clone());
        self.completed.insert(
            key,
            CompletedScan {
                stamp,
                unknown_plans: result.unknown_plans,
                unknown_tasks: result.unknown_tasks,
                warnings: scan.warnings,
            },
        );
        Ok(())
    }
}

fn record_scan(
    backend: &mut dyn BoardBackend,
    actor: &BoardActor,
    registration: &RepoRegistration,
    error: Option<String>,
) -> Result<()> {
    let reply = backend.handle(&BoardRequest::new(
        actor.clone(),
        BoardOp::RecordScan {
            repo_key: registration.repo_key.clone(),
            host: registration.host.clone(),
            common_dir: registration.common_dir.clone(),
            error,
        },
    ))?;
    ensure!(
        matches!(reply.result, BoardResult::ScanRecorded),
        "board_scan: unexpected scan-status reply"
    );
    Ok(())
}

pub fn find_unlinked(
    registration: &RepoRegistration,
    since: i64,
    agent: &HarnessLabel,
    timeout: Duration,
) -> Result<UnlinkedScan> {
    let deadline = Instant::now() + timeout;
    let tips = collect_tips(registration, deadline)?;
    let scan = scan_log(registration, &tips, since, false, deadline)?;
    let mut scan_error = (!scan.complete)
        .then(|| format!("board_scan: unlinked scan exceeds {COMMIT_LIMIT} records"));
    if collect_tips(registration, deadline)?.digest != tips.digest {
        scan_error = Some("board_scan: Git tips changed during unlinked scan".to_owned());
    }
    let commits = scan
        .records
        .into_iter()
        .filter(|record| !record.has_plan_trailer && attributed_to(&record.commit, agent))
        .map(|record| record.commit)
        .collect();
    Ok(UnlinkedScan {
        commits,
        #[cfg(test)]
        complete: scan_error.is_none(),
        scan_error: scan_error
            .or_else(|| (!scan.warnings.is_empty()).then(|| scan.warnings.join("; "))),
    })
}

/// Reads one commit's metadata exactly as a scan does, without trailer links.
pub fn read_commit(
    registration: &RepoRegistration,
    oid: crate::identity::GitOid,
    timeout: Duration,
) -> Result<LinkedCommit> {
    commit_scanner::read_commit(registration, oid, Instant::now() + timeout)
}
