//! Local Git scans; only accepted complete scans advance edge-local digests.
use super::board_actor::{BoardActor, HarnessLabel};
use super::board_backend::BoardBackend;
use super::board_ids::{PlanId, RepoKey, TaskId};
use super::board_protocol::{
    BoardOp, BoardRequest, BoardResult, LinkedCommit, RepoRegistration, RepoScanTarget,
};
use super::commit_trailers::{LOG_FORMAT, ParsedCommit, attributed_to, parse_log};
use super::repo_identity::{detached_head, remaining};
use crate::history::git::run_bounded_strict as run_bounded;
use crate::identity::GitOid;
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const COMMIT_LIMIT: usize = 2000;
const TIP_LIMIT: usize = 4096;
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
                    report.errors.push(format!("{}: repository common directory is missing; cleanup waits for grace period", target.registration.common_dir.display()));
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
    let mut complete = scan.complete;
    let mut scan_error =
        (!complete).then(|| format!("board_scan: unlinked scan exceeds {COMMIT_LIMIT} records"));
    if collect_tips(registration, deadline)?.digest != tips.digest {
        complete = false;
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
        complete,
        scan_error: scan_error
            .or_else(|| (!scan.warnings.is_empty()).then(|| scan.warnings.join("; "))),
    })
}

struct GitTips {
    detached: Vec<GitOid>,
    digest: [u8; 32],
    is_empty: bool,
}

fn collect_tips(registration: &RepoRegistration, deadline: Instant) -> Result<GitTips> {
    let common = &registration.common_dir;
    let refs = run_bounded(
        common,
        &["for-each-ref", "--format=%(objectname)", "refs/heads"],
        remaining(deadline)?,
    )?;
    let mut all = BTreeSet::new();
    for oid in std::str::from_utf8(&refs)?.lines() {
        all.insert(GitOid::parse(oid)?);
    }
    ensure!(
        all.len() <= TIP_LIMIT,
        "board_scan: branch tips exceed resource limit"
    );
    let mut detached = BTreeSet::new();
    add_detached(common, &mut detached, deadline)?;
    let worktrees = match std::fs::read_dir(common.join("worktrees")) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("board_scan: enumerate linked worktrees"),
    };
    let mut count = 0;
    for entry in worktrees.into_iter().flatten() {
        remaining(deadline)?;
        count += 1;
        ensure!(
            count <= TIP_LIMIT,
            "board_scan: linked worktrees exceed resource limit"
        );
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !entry.path().join("HEAD").is_file() {
            continue;
        }
        add_detached(&entry.path(), &mut detached, deadline)?;
    }
    all.extend(detached.iter().copied());
    ensure!(
        all.len() <= TIP_LIMIT,
        "board_scan: repository tips exceed resource limit"
    );
    let mut hasher = blake3::Hasher::new();
    for oid in &all {
        hasher.update(oid.as_str().as_bytes());
        hasher.update(b"\n");
    }
    Ok(GitTips {
        detached: detached.into_iter().collect(),
        digest: *hasher.finalize().as_bytes(),
        is_empty: all.is_empty(),
    })
}

fn add_detached(
    path: &std::path::Path,
    tips: &mut BTreeSet<GitOid>,
    deadline: Instant,
) -> Result<()> {
    if let Some(head) = detached_head(path, deadline)? {
        tips.insert(head);
    }
    Ok(())
}

struct LogScan {
    records: Vec<ParsedCommit>,
    complete: bool,
    warnings: Vec<String>,
}

fn scan_log(
    registration: &RepoRegistration,
    tips: &GitTips,
    since: i64,
    linked_only: bool,
    deadline: Instant,
) -> Result<LogScan> {
    if tips.is_empty {
        return Ok(LogScan {
            records: Vec::new(),
            complete: true,
            warnings: Vec::new(),
        });
    }
    let since = format!("--since=@{since}");
    let cap = format!("--max-count={}", COMMIT_LIMIT + 1);
    let mut args = Vec::with_capacity(tips.detached.len() + 12);
    args.extend([
        "log",
        "--branches",
        "--no-decorate",
        "--no-color",
        "--no-notes",
        "--no-renames",
    ]);
    args.extend(tips.detached.iter().map(GitOid::as_str));
    args.extend([since.as_str(), cap.as_str(), LOG_FORMAT]);
    if linked_only {
        args.extend(["-E", "-i", "--grep=^Plan(-Task)?[[:space:]]*:"]);
    }
    args.push("--");
    let bytes = run_bounded(&registration.common_dir, &args, remaining(deadline)?)?;
    let parsed = parse_log(&bytes, &registration.repo_key)?;
    let complete = parsed.record_count <= COMMIT_LIMIT;
    let mut records = parsed.records;
    records.truncate(COMMIT_LIMIT);
    let mut warnings = parsed.warnings;
    let stats_budget = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(Duration::from_millis(500))
        .min(Duration::from_secs(1));
    if !records.is_empty() && !stats_budget.is_zero() {
        let mut stats_args = args.clone();
        stats_args.insert(1, "--shortstat");
        match run_bounded(&registration.common_dir, &stats_args, stats_budget)
            .and_then(|bytes| parse_log(&bytes, &registration.repo_key))
        {
            Ok(stats) => {
                let statistics = stats
                    .records
                    .into_iter()
                    .map(|record| (record.commit.oid, record.commit))
                    .collect::<HashMap<_, _>>();
                for record in &mut records {
                    if let Some(stats) = statistics.get(&record.commit.oid) {
                        record.commit.files = stats.files;
                        record.commit.insertions = stats.insertions;
                        record.commit.deletions = stats.deletions;
                    }
                }
            }
            Err(error) => warnings.push(format!(
                "board_scan: commit statistics unavailable: {error:#}"
            )),
        }
    }
    Ok(LogScan {
        records,
        complete,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::super::board_ids::EventSeq;
    use super::super::board_protocol::{BoardError, BoardReply, CommitLinkResult};
    use super::super::repo_identity::tests::GitFixture;
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[derive(Default)]
    struct TestBackend {
        linked: Vec<LinkedCommit>,
        requests: usize,
        fail_next_link: bool,
        unknown_plans: Vec<PlanId>,
        scan_errors: Vec<Option<String>>,
        forgotten: usize,
        fail_forget: bool,
    }
    impl BoardBackend for TestBackend {
        fn handle(
            &mut self,
            request: &BoardRequest,
        ) -> std::result::Result<BoardReply, BoardError> {
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
        fn max_seq(&self) -> std::result::Result<EventSeq, BoardError> {
            Ok(EventSeq::new(0))
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

    #[test]
    fn native_trailers_link_case_insensitively_and_exclude_body_mentions() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        fixture.commit("body mention\n\nPlan: P7\n\nThis is body prose, not a trailer.");
        std::fs::write(fixture.root.join("source.txt"), "one\ntwo\n").unwrap();
        fixture.git(&["add", "source.txt"]);
        let linked = fixture.commit("real footer\n\nPLAN: P7\nPlan-Task: P7.3\nCo-Authored-By: Model Claim <noreply@OpenAI.com>");
        let folded = fixture.commit("folded footer\n\nPlan:\n P7\nPlan-Task:\n P7.3");
        let spaced = fixture.commit("spaced footer\n\nPlan : P7");
        let target = target(&fixture);
        let mut backend = TestBackend::default();
        let report = RepoIngestor::default()
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            backend
                .linked
                .iter()
                .map(|commit| commit.oid)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([linked, folded, spaced])
        );
        let linked = backend
            .linked
            .iter()
            .find(|commit| commit.oid == linked)
            .unwrap();
        assert_eq!(linked.plans[0].task_ordinal, Some(3));
        assert_eq!(linked.coauthors[0].harness.as_str(), "codex");
        assert_eq!(linked.files, 1);
        assert_eq!(linked.insertions, 2);
    }

    #[test]
    fn branch_and_main_and_linked_detached_commits_are_all_scanned() {
        let fixture = GitFixture::new();
        let root = fixture.commit("root");
        fixture.git(&["checkout", "--quiet", "-b", "other"]);
        let branch = fixture.commit(&linked_message("noncurrent branch"));
        fixture.git(&["checkout", "--quiet", "--detach", root.as_str()]);
        let main_detached = fixture.commit(&linked_message("main detached"));
        let worktree = fixture.directory.path().join("linked");
        fixture.git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().unwrap(),
            root.as_str(),
        ]);
        fixture.git_in(
            &worktree,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                &linked_message("linked detached"),
            ],
        );
        let linked_detached =
            GitOid::parse(fixture.git_in(&worktree, &["rev-parse", "HEAD"]).trim()).unwrap();
        let target = target(&fixture);
        let mut backend = TestBackend::default();
        let mut ingestor = RepoIngestor::default();
        let report = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[target.clone()],
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let actual: BTreeSet<_> = backend.linked.iter().map(|commit| commit.oid).collect();
        assert_eq!(
            actual,
            BTreeSet::from([branch, main_detached, linked_detached])
        );
        let report = ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap();
        assert_eq!(report.skipped, 1);
        assert_eq!(backend.requests, 1);
    }

    #[test]
    fn missing_common_directory_is_removed_only_after_grace() {
        let fixture = GitFixture::new();
        fixture.commit(&linked_message("linked root"));
        let target = target(&fixture);
        let mut backend = TestBackend::default();
        let mut ingestor = RepoIngestor::default();
        std::fs::remove_dir_all(&target.registration.common_dir).unwrap();
        let first = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[target.clone()],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(first.skipped, 1);
        assert_eq!(backend.forgotten, 0);
        let key = (
            target.registration.repo_key.clone(),
            target.registration.common_dir.clone(),
        );
        ingestor
            .missing
            .insert(key, Instant::now() - MISSING_REPOSITORY_GRACE);
        let second = ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap();
        assert_eq!(second.skipped, 1);
        assert_eq!(backend.forgotten, 1);
        assert!(ingestor.missing.is_empty());
    }

    #[test]
    fn failed_missing_path_cleanup_does_not_block_healthy_repositories() {
        let missing = GitFixture::new();
        missing.commit(&linked_message("missing root"));
        let missing_target = target(&missing);
        std::fs::remove_dir_all(&missing_target.registration.common_dir).unwrap();
        let healthy = GitFixture::new();
        healthy.commit(&linked_message("healthy root"));
        let mut backend = TestBackend {
            fail_forget: true,
            ..TestBackend::default()
        };
        let mut ingestor = RepoIngestor::default();
        let key = (
            missing_target.registration.repo_key.clone(),
            missing_target.registration.common_dir.clone(),
        );
        ingestor
            .missing
            .insert(key.clone(), Instant::now() - MISSING_REPOSITORY_GRACE);
        let report = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[missing_target, target(&healthy)],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.completed.len(), 1);
        assert_eq!(backend.linked.len(), 1);
        assert!(ingestor.missing.contains_key(&key));
    }

    #[test]
    fn stray_linked_worktree_files_do_not_abort_scans() {
        let fixture = GitFixture::new();
        fixture.commit(&linked_message("linked root"));
        std::fs::create_dir_all(fixture.root.join(".git/worktrees")).unwrap();
        std::fs::write(fixture.root.join(".git/worktrees/stray"), "unrelated").unwrap();
        std::fs::create_dir(fixture.root.join(".git/worktrees/empty-stray")).unwrap();
        let mut backend = TestBackend::default();
        let report = RepoIngestor::default()
            .ingest(
                &mut backend,
                &actor(),
                &[target(&fixture)],
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed.len(), 1);
        assert_eq!(backend.linked.len(), 1);
    }

    #[test]
    fn failed_backend_link_keeps_same_tip_scan_retryable() {
        let fixture = GitFixture::new();
        fixture.commit(&linked_message("linked root"));
        let target = target(&fixture);
        let mut ingestor = RepoIngestor::default();
        let mut backend = TestBackend {
            fail_next_link: true,
            ..TestBackend::default()
        };
        let first = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[target.clone()],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(first.errors.len(), 1);
        assert!(backend.scan_errors[0].is_some());
        let second = ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap();
        assert!(second.errors.is_empty());
        assert_eq!(backend.requests, 2);
        assert_eq!(second.inserted, 1);
        assert_eq!(backend.scan_errors.last(), Some(&None));
    }

    #[test]
    fn unknown_plan_advances_stamp_until_plan_set_changes() {
        let fixture = GitFixture::new();
        fixture.commit("future plan\n\nPlan: P99");
        let mut target = target(&fixture);
        let unknown = PlanId::new(99).unwrap();
        let mut backend = TestBackend {
            unknown_plans: vec![unknown],
            ..TestBackend::default()
        };
        let mut ingestor = RepoIngestor::default();
        let first = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[target.clone()],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(first.unknown_plans, vec![unknown]);
        let second = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[target.clone()],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(second.skipped, 1);
        assert_eq!(backend.requests, 1);
        assert_eq!(second.unknown_plans, vec![unknown]);
        backend.unknown_plans.clear();
        target.plans.push(unknown);
        let third = ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap();
        assert_eq!(third.scanned, 1);
        assert_eq!(backend.requests, 2);
    }

    #[test]
    fn malformed_commit_is_isolated_and_valid_metadata_is_bounded() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        fixture.commit("bad author\n\nPlan: P7\nCo-authored-by: broken");
        fixture.commit("bad task\n\nPlan: P7\nPlan-Task: invalid");
        let subject = "é".repeat(900);
        fixture.git(&["config", "user.name", &"é".repeat(900)]);
        let good = fixture.commit(&format!(
            "{subject}\n\nPlan: P7\nPlan-Task: P7.3\nPlan-Task: P7.4"
        ));
        let target = target(&fixture);
        let mut ingestor = RepoIngestor::default();
        let mut backend = TestBackend::default();
        let first = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[target.clone()],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(first.completed.len(), 1);
        assert_eq!(first.errors.len(), 2);
        assert_eq!(backend.linked.len(), 1);
        let commit = &backend.linked[0];
        assert_eq!(commit.oid, good);
        assert!(commit.subject.len() <= 1024);
        assert!(commit.author.len() <= 1024);
        assert_eq!(
            commit
                .plans
                .iter()
                .map(|link| link.task_ordinal)
                .collect::<Vec<_>>(),
            vec![Some(3), Some(4)]
        );
        BoardRequest::new(
            actor(),
            BoardOp::LinkCommits {
                commits: backend.linked.clone(),
            },
        )
        .validate()
        .unwrap();
        assert_eq!(
            ingestor
                .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
                .unwrap()
                .skipped,
            1
        );
    }

    #[test]
    fn missing_blob_statistics_do_not_block_commit_links_or_stamp() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        std::fs::write(
            fixture.root.join("unavailable.txt"),
            "missing object contents\n",
        )
        .unwrap();
        fixture.git(&["add", "unavailable.txt"]);
        let blob = fixture.git(&["rev-parse", ":unavailable.txt"]);
        let blob = blob.trim();
        let linked = fixture.commit(&linked_message("missing blob"));
        std::fs::remove_file(
            fixture
                .root
                .join(".git/objects")
                .join(&blob[..2])
                .join(&blob[2..]),
        )
        .unwrap();
        let target = target(&fixture);
        let mut backend = TestBackend::default();
        let mut ingestor = RepoIngestor::default();
        let first = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[target.clone()],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(first.completed.len(), 1);
        assert_eq!(backend.linked.len(), 1);
        assert_eq!(backend.linked[0].oid, linked);
        assert_eq!(backend.linked[0].files, 0);
        assert!(
            first
                .errors
                .iter()
                .any(|warning| warning.contains("statistics"))
        );
        assert_eq!(
            ingestor
                .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
                .unwrap()
                .skipped,
            1
        );
    }

    #[test]
    fn planless_targets_are_skipped_without_reading_git() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        let mut target = target(&fixture);
        target.plans.clear();
        target.registration.common_dir = fixture.root.join("missing");
        let mut backend = TestBackend::default();
        let report = RepoIngestor::default()
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap();
        assert_eq!(report.skipped, 1);
        assert!(report.errors.is_empty());
        assert_eq!(backend.requests, 0);
    }

    #[test]
    fn unlinked_lookup_filters_by_vendor_and_true_plan_trailer_presence() {
        let fixture = GitFixture::new();
        fixture.commit("human root");
        let unlinked =
            fixture.commit("unlinked\n\nCo-authored-by: Codex claim <noreply@openai.com>");
        fixture.commit(&linked_message("linked"));
        fixture.commit("other vendor\n\nCo-authored-by: Claude claim <noreply@anthropic.com>");
        fixture.commit(
            "invalid link\n\nPlan: not-a-plan\nCo-authored-by: Codex claim <noreply@openai.com>",
        );
        let task_only = fixture.commit(
            "task only\n\nPlan-Task: P7.3\nCo-authored-by: Codex claim <noreply@openai.com>",
        );
        let scan = find_unlinked(
            &fixture.registration(),
            1_700_000_000,
            &HarnessLabel::parse("codex").unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(scan.complete);
        assert_eq!(scan.commits.len(), 2);
        assert_eq!(
            scan.commits
                .iter()
                .map(|commit| commit.oid)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([unlinked, task_only])
        );
    }

    #[test]
    fn capped_scan_links_bounded_records_without_advancing_digest() {
        let fixture = GitFixture::new();
        let message = linked_message("bulk linked");
        let mut stream = Vec::new();
        for ordinal in 0..=COMMIT_LIMIT {
            writeln!(stream, "commit refs/heads/main\ncommitter Fixture <fixture@example.test> {} +0000\ndata {}\n{}\n", 1_700_000_000 + ordinal, message.len(), message).unwrap();
        }
        let mut child = Command::new("git")
            .current_dir(&fixture.root)
            .args(["fast-import", "--quiet"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&stream).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let target = target(&fixture);
        let mut ingestor = RepoIngestor::default();
        let mut backend = TestBackend::default();
        for _ in 0..2 {
            let report = ingestor
                .ingest(
                    &mut backend,
                    &actor(),
                    &[target.clone()],
                    Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(report.inserted, COMMIT_LIMIT as u64);
            assert_eq!(report.errors.len(), 1);
        }
        assert_eq!(backend.requests, 2);
        assert!(ingestor.completed.is_empty());
    }

    #[test]
    fn broken_git_reference_warning_prevents_complete_scan() {
        let fixture = GitFixture::new();
        fixture.commit(&linked_message("valid root"));
        let target = target(&fixture);
        std::fs::write(
            target.registration.common_dir.join("refs/heads/broken"),
            "not-an-oid\n",
        )
        .unwrap();
        let mut backend = TestBackend::default();
        let mut ingestor = RepoIngestor::default();
        let report = ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap();
        assert_eq!(backend.requests, 0);
        assert!(report.errors[0].contains("warning prevents a complete scan"));
        assert!(ingestor.completed.is_empty());
    }
}
