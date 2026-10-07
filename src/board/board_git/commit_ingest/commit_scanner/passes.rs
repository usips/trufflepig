//! Body-free second passes: per-commit statistics and misplaced-trailer grep.
use crate::board::commit_trailers::shortstat;
use crate::identity::GitOid;
use anyhow::{Result, ensure};
use std::collections::HashMap;

/// Body-free oid plus shortstat pairing for the bounded statistics pass.
pub(super) const STATS_FORMAT: &str = "--format=%x00%H%x00";
/// A trailer-shaped line Git's final-paragraph rule ignored still matches this
/// grep; its parsed plan trailers stay empty, so the commit warns, never links.
pub(super) const GREP_FORMAT: &str = concat!(
    "--format=%x00%H%x00%s%x00",
    "%(trailers:key=Plan,valueonly)%x00",
    "%(trailers:key=Plan-Task,valueonly)%x00"
);
pub(super) const MISPLACED_PATTERN: &str = "^(Plan|Plan-Task):[[:space:]]*P[0-9]";

/// One bounded `git log` invocation over the scanned range and tip set.
pub(super) fn log_args<'a>(
    detached: &'a [GitOid],
    since: &'a str,
    cap: &'a str,
    format: &'a str,
    extra: &[&'a str],
) -> Vec<&'a str> {
    let mut args = Vec::with_capacity(detached.len() + 12 + extra.len());
    args.extend([
        "log",
        "--branches",
        "--no-decorate",
        "--no-color",
        "--no-notes",
        "--no-renames",
    ]);
    args.extend(detached.iter().map(GitOid::as_str));
    args.extend([since, cap, format]);
    args.extend(extra.iter().copied());
    args.push("--");
    args
}

/// Body-free per-commit statistics keyed by oid; bad records default to zero.
pub(super) fn parse_stats(bytes: &[u8]) -> Result<HashMap<GitOid, (u64, u64, u64)>> {
    if bytes.is_empty() {
        return Ok(HashMap::new());
    }
    ensure!(bytes[0] == 0, "board_scan: invalid Git stats framing");
    let fields: Vec<_> = bytes[1..].split(|byte| *byte == 0).collect();
    ensure!(
        fields.len() % 2 == 0,
        "board_scan: incomplete Git stats record"
    );
    let mut stats = HashMap::with_capacity(fields.len() / 2);
    for record in fields.chunks_exact(2) {
        let oid = GitOid::parse(std::str::from_utf8(record[0])?)?;
        let text = String::from_utf8_lossy(record[1]);
        stats.insert(oid, shortstat(&text).unwrap_or_default());
    }
    Ok(stats)
}

/// Candidates have no parsed plan trailer and no matching subject; a subject
/// match is conservatively skipped because grep also searches that line.
pub(super) fn parse_grep_log(bytes: &[u8]) -> Result<Vec<GitOid>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(bytes[0] == 0, "board_scan: invalid Git grep framing");
    let fields: Vec<_> = bytes[1..].split(|byte| *byte == 0).collect();
    ensure!(
        fields.len() % 5 == 0,
        "board_scan: incomplete Git grep record"
    );
    let mut oids = Vec::with_capacity(fields.len() / 5);
    for record in fields.chunks_exact(5) {
        let oid = GitOid::parse(std::str::from_utf8(record[0])?)?;
        if record[2].is_empty()
            && record[3].is_empty()
            && !subject_matches(&String::from_utf8_lossy(record[1]))
        {
            oids.push(oid);
        }
    }
    Ok(oids)
}

fn subject_matches(subject: &str) -> bool {
    let Some((key, value)) = subject.split_once(':') else {
        return false;
    };
    let value = value.trim_start().as_bytes();
    (key.eq_ignore_ascii_case("Plan") || key.eq_ignore_ascii_case("Plan-Task"))
        && value
            .first()
            .is_some_and(|byte| byte.to_ascii_uppercase() == b'P')
        && value.get(1).is_some_and(u8::is_ascii_digit)
}
