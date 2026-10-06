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
    "--format=%x00%H%x00",
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

/// One warning per oid the misplaced-trailer grep matched while both parsed
/// plan trailer fields stayed empty.
pub(super) fn parse_grep_log(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(bytes[0] == 0, "board_scan: invalid Git grep framing");
    let fields: Vec<_> = bytes[1..].split(|byte| *byte == 0).collect();
    ensure!(
        fields.len() % 4 == 0,
        "board_scan: incomplete Git grep record"
    );
    let mut warnings = Vec::new();
    for record in fields.chunks_exact(4) {
        if record[1].is_empty() && record[2].is_empty() {
            warnings.push(format!(
                "board_scan: {}: misplaced_trailers",
                String::from_utf8_lossy(record[0])
            ));
        }
    }
    Ok(warnings)
}
