//! Local Git scans; only accepted complete scans advance edge-local digests.
use super::board_actor::{BoardActor, HarnessLabel};
use super::board_backend::BoardBackend;
use super::board_ids::{PlanId, RepoKey};
use super::board_protocol::{
    BoardOp, BoardRequest, BoardResult, LinkedCommit, RepoRegistration, RepoScanTarget,
};
use super::commit_trailers::{LOG_FORMAT, ParsedCommit, attributed_to, parse_log};
use super::repo_identity::{read_head, remaining};
use crate::history::git::run_bounded_strict as run_bounded;
use crate::identity::GitOid;
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const COMMIT_LIMIT: usize = 2000;
const TIP_LIMIT: usize = 4096;

#[derive(Clone, Debug, Default)]
pub struct IngestReport {
    pub scanned: u64,
    pub skipped: u64,
    pub inserted: u64,
    pub unknown_plans: Vec<PlanId>,
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
    completed: HashMap<(RepoKey, PathBuf), ScanStamp>,
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
        if self.completed.get(&key) == Some(&stamp) && target.scan_error.is_none() {
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
        let has_unknown = !result.unknown_plans.is_empty();
        report.unknown_plans.extend(result.unknown_plans);
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
        if !has_unknown {
            self.completed.insert(key, stamp);
        }
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
        scan_error,
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
    add_detached(&common.join("HEAD"), &mut detached)?;
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
        ensure!(
            entry.file_type()?.is_dir(),
            "board_scan: invalid linked-worktree metadata"
        );
        add_detached(&entry.path().join("HEAD"), &mut detached)?;
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

fn add_detached(path: &std::path::Path, tips: &mut BTreeSet<GitOid>) -> Result<()> {
    let head = read_head(path)?;
    let head = head.trim();
    if head.starts_with("ref: refs/") {
        return Ok(());
    }
    tips.insert(GitOid::parse(head)?);
    Ok(())
}

struct LogScan {
    records: Vec<ParsedCommit>,
    complete: bool,
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
    ]);
    args.extend(tips.detached.iter().map(GitOid::as_str));
    args.extend([since.as_str(), cap.as_str(), "--shortstat", LOG_FORMAT]);
    if linked_only {
        args.extend(["-E", "-i", "--grep=^Plan(-Task)?[[:space:]]*:"]);
    }
    args.push("--");
    let bytes = run_bounded(&registration.common_dir, &args, remaining(deadline)?)?;
    let mut records = parse_log(&bytes, &registration.repo_key)?;
    let complete = records.len() <= COMMIT_LIMIT;
    records.truncate(COMMIT_LIMIT);
    Ok(LogScan { records, complete })
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
                    })
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
    fn unknown_plan_keeps_same_tip_scan_retryable_until_resolved() {
        let fixture = GitFixture::new();
        fixture.commit("future plan\n\nPlan: P99");
        let target = target(&fixture);
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
        assert!(ingestor.completed.is_empty());
        backend.unknown_plans.clear();
        let second = ingestor
            .ingest(
                &mut backend,
                &actor(),
                &[target.clone()],
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(second.unknown_plans.is_empty());
        assert!(second.errors.is_empty());
        assert_eq!(backend.requests, 2);
        let third = ingestor
            .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
            .unwrap();
        assert_eq!(third.skipped, 1);
        assert_eq!(backend.requests, 2);
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
