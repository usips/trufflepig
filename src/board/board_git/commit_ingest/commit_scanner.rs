//! Deadline-bound ref discovery and complete, bounded commit scans.
mod passes;

use super::COMMIT_LIMIT;
use crate::board::{
    board_protocol::{LinkedCommit, RepoRegistration},
    commit_trailers::{LOG_FORMAT, ParsedCommit, parse_log},
    repo_identity::{detached_head, remaining},
};
use crate::history::git::{run_bounded, run_bounded_strict};
use crate::identity::GitOid;
use anyhow::{Context, Result, ensure};
use passes::{GREP_FORMAT, MISPLACED_PATTERN, STATS_FORMAT, log_args, parse_grep_log, parse_stats};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const TIP_LIMIT: usize = 4096;
const AUXILIARY_BUDGET: Duration = Duration::from_secs(1);
const FINAL_SCAN_RESERVE: Duration = Duration::from_millis(500);

const GREP_PATTERN: &str = "^Plan(-Task)?[[:space:]]*:";

#[cfg(test)]
thread_local! {
    static EXPIRE_AFTER_MAIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn expire_next_auxiliary_budget() {
    EXPIRE_AFTER_MAIN.with(|expire| expire.set(true));
}

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

/// Reads one commit's metadata exactly as a scan does, without trailer links.
pub(super) fn read_commit(
    registration: &RepoRegistration,
    oid: GitOid,
    deadline: Instant,
) -> Result<LinkedCommit> {
    let args = [
        "log",
        "-1",
        "--no-decorate",
        "--no-color",
        "--no-notes",
        "--no-renames",
        LOG_FORMAT,
        oid.as_str(),
        "--",
    ];
    let bytes = run_bounded(&registration.common_dir, &args, remaining(deadline)?)?;
    let mut parsed = parse_log(&bytes, &registration.repo_key)?;
    ensure!(
        parsed.record_count == 1 && parsed.records.len() == 1,
        "board_link: commit metadata is unavailable"
    );
    let stats_budget = remaining(deadline)?.min(Duration::from_secs(1));
    if !stats_budget.is_zero() {
        let stats_args = [
            "log",
            "--shortstat",
            "-1",
            "--no-decorate",
            "--no-color",
            "--no-notes",
            "--no-renames",
            STATS_FORMAT,
            oid.as_str(),
            "--",
        ];
        if let Ok(stats) = run_bounded(&registration.common_dir, &stats_args, stats_budget)
            .and_then(|bytes| parse_stats(&bytes))
            && let Some(&(files, insertions, deletions)) = stats.get(&oid)
        {
            let record = &mut parsed.records[0].commit;
            record.files = files;
            record.insertions = insertions;
            record.deletions = deletions;
        }
    }
    let mut record = parsed.records.into_iter().next().expect("one record");
    record.commit.plans = Vec::new();
    Ok(record.commit)
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
    let mut filter = Vec::new();
    if linked_only {
        filter.extend(["-E", "-i", "--grep", GREP_PATTERN]);
    }
    // Per-commit passes tolerate Git warnings: NUL framing and the strict tip
    // digest re-check still gate scan completion.
    let bytes = run_bounded(
        &registration.common_dir,
        &log_args(&tips.detached, &since, &cap, LOG_FORMAT, &filter),
        remaining(deadline)?,
    )?;
    let parsed = parse_log(&bytes, &registration.repo_key)?;
    let complete = parsed.record_count <= COMMIT_LIMIT;
    let mut records = parsed.records;
    records.truncate(COMMIT_LIMIT);
    let mut warnings = parsed.warnings;
    #[cfg(test)]
    let deadline = EXPIRE_AFTER_MAIN.with(|expire| {
        if expire.replace(false) {
            Instant::now()
        } else {
            deadline
        }
    });
    let grep_budget = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(AUXILIARY_BUDGET + FINAL_SCAN_RESERVE)
        .min(AUXILIARY_BUDGET);
    if grep_budget.is_zero() {
        warnings.push(
            "board_scan: trailer_check_skipped: misplaced-trailer scan budget exhausted".into(),
        );
    } else {
        match run_bounded(
            &registration.common_dir,
            &log_args(
                &tips.detached,
                &since,
                &cap,
                GREP_FORMAT,
                &["-E", "-i", "--grep", MISPLACED_PATTERN],
            ),
            grep_budget,
        )
        .and_then(|bytes| parse_grep_log(&bytes))
        {
            Ok(misplaced) => warnings.extend(
                misplaced
                    .into_iter()
                    .map(|oid| format!("board_scan: {oid}: misplaced_trailers")),
            ),
            Err(error) => warnings.push(format!(
                "board_scan: trailer_check_skipped: misplaced-trailer scan unavailable: {error:#}"
            )),
        }
    }
    let stats_budget = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(FINAL_SCAN_RESERVE)
        .min(AUXILIARY_BUDGET);
    if !records.is_empty() && !stats_budget.is_zero() {
        let mut stats_filter = filter.clone();
        stats_filter.insert(0, "--shortstat");
        match run_bounded(
            &registration.common_dir,
            &log_args(&tips.detached, &since, &cap, STATS_FORMAT, &stats_filter),
            stats_budget,
        )
        .and_then(|bytes| parse_stats(&bytes))
        {
            Ok(stats) => {
                for record in &mut records {
                    if let Some(&(files, insertions, deletions)) = stats.get(&record.commit.oid) {
                        record.commit.files = files;
                        record.commit.insertions = insertions;
                        record.commit.deletions = deletions;
                    }
                }
            }
            Err(error) => warnings.push(format!(
                "board_scan: commit statistics unavailable: {error:#}"
            )),
        }
    }
    let mut seen = std::collections::HashSet::with_capacity(warnings.len());
    warnings.retain(|warning| seen.insert(warning.clone()));
    Ok(LogScan {
        records,
        complete,
        warnings,
    })
}
