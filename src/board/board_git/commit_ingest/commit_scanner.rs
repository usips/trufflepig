//! Deadline-bound ref discovery and complete, bounded commit scans.
use super::COMMIT_LIMIT;
use crate::board::{
    board_protocol::RepoRegistration,
    commit_trailers::{LOG_FORMAT, ParsedCommit, parse_log},
    repo_identity::{detached_head, remaining},
};
use crate::history::git::{run_bounded, run_bounded_strict};
use crate::identity::GitOid;
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

const TIP_LIMIT: usize = 4096;

pub(super) struct GitTips {
    detached: Vec<GitOid>,
    pub(super) digest: [u8; 32],
    is_empty: bool,
}

/// Ref discovery stays strict: a ref warning can hide an entire branch tip.
pub(super) fn collect_tips(registration: &RepoRegistration, deadline: Instant) -> Result<GitTips> {
    let common = &registration.common_dir;
    let refs = run_bounded_strict(
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

pub(super) struct LogScan {
    pub(super) records: Vec<ParsedCommit>,
    pub(super) complete: bool,
    pub(super) warnings: Vec<String>,
}

pub(super) fn scan_log(
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
    // Per-commit passes tolerate Git warnings: NUL framing and the strict tip
    // digest re-check still gate scan completion.
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
