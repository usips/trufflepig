//! One NUL-framed log record parsed into a linked commit with warnings.
use super::{ParsedCommit, parse_coauthor};
use crate::board::board_ids::{PlanId, RepoKey, TaskId};
use crate::board::board_protocol::{
    COAUTHOR_LIMIT, CommitCoauthor, CommitPlanLink, LINK_LIMIT, LinkedCommit,
};
use crate::identity::GitOid;
use anyhow::{Context, Result, ensure};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::str::FromStr;

pub(super) struct ParsedLogRecord {
    pub(super) commit: ParsedCommit,
    pub(super) warnings: Vec<String>,
}

pub(super) fn parse_record(fields: &[&[u8]], repo_key: &RepoKey) -> Result<ParsedLogRecord> {
    let mut warnings = Vec::new();
    let oid = std::str::from_utf8(fields[0])?;
    let timestamp = std::str::from_utf8(fields[1])?;
    let author = lossy_field(fields[2], "author", &mut warnings);
    let subject = lossy_field(fields[3], "subject", &mut warnings);
    let plan_field = lossy_field(fields[4], "Plan trailer", &mut warnings);
    let task_field = lossy_field(fields[5], "Plan-Task trailer", &mut warnings);
    let coauthor_field = lossy_field(fields[6], "Co-authored-by trailer", &mut warnings);
    let shortstat_field = lossy_field(fields[7], "shortstat", &mut warnings);
    let raw_plans = trailer_values(&plan_field, "Plan")?;
    let has_plan_trailer = !raw_plans.is_empty();
    let raw_tasks = trailer_values(&task_field, "Plan-Task")?;
    let plans: BTreeSet<PlanId> = trailer_ids(raw_plans, "Plan", &mut warnings);
    let tasks: BTreeSet<TaskId> = trailer_ids(raw_tasks, "Plan-Task", &mut warnings);
    if tasks.iter().any(|task| !plans.contains(&task.plan)) {
        warnings.push("plan_task_without_plan".to_owned());
    }
    let mut links = Vec::with_capacity(plans.len() + tasks.len());
    for plan in &plans {
        let before = links.len();
        for task in tasks.iter().filter(|task| task.plan == *plan) {
            links.push(CommitPlanLink {
                plan_id: *plan,
                task_ordinal: Some(task.ordinal),
            });
        }
        if links.len() == before {
            links.push(CommitPlanLink {
                plan_id: *plan,
                task_ordinal: None,
            });
        }
    }
    if links.len() > LINK_LIMIT {
        warnings.push(format!(
            "skipped {} plan reference(s) past the {LINK_LIMIT} limit",
            links.len() - LINK_LIMIT
        ));
        links.truncate(LINK_LIMIT);
    }
    let mut coauthors: Vec<CommitCoauthor> = Vec::new();
    let mut malformed = 0usize;
    let mut first_malformed: Option<anyhow::Error> = None;
    let mut overflow = 0usize;
    for value in trailer_values(&coauthor_field, "Co-authored-by")? {
        match parse_coauthor(value) {
            Ok(coauthor) if coauthors.contains(&coauthor) => {}
            Ok(_) if coauthors.len() >= COAUTHOR_LIMIT => overflow += 1,
            Ok(coauthor) => coauthors.push(coauthor),
            Err(error) => {
                malformed += 1;
                first_malformed.get_or_insert(error);
            }
        }
    }
    if let Some(error) = first_malformed {
        warnings.push(format!(
            "skipped {malformed} malformed co-author trailer(s): {error:#}"
        ));
    }
    if overflow > 0 {
        warnings.push(format!(
            "skipped {overflow} co-author trailer(s) past the {COAUTHOR_LIMIT} limit"
        ));
    }
    let (files, insertions, deletions) = shortstat(&shortstat_field).unwrap_or_default();
    Ok(ParsedLogRecord {
        warnings,
        commit: ParsedCommit {
            has_plan_trailer,
            commit: LinkedCommit {
                repo_key: repo_key.clone(),
                oid: GitOid::parse(oid)?,
                committed_at: timestamp
                    .parse()
                    .context("board_scan: invalid commit timestamp")?,
                author: bounded_git_metadata(&author, 1024),
                subject: bounded_git_metadata(&subject, 1024),
                coauthors,
                files,
                insertions,
                deletions,
                plans: links,
            },
        },
    })
}

/// Metadata fields decode lossily so one bad byte never drops a record.
fn lossy_field<'a>(field: &'a [u8], name: &str, warnings: &mut Vec<String>) -> Cow<'a, str> {
    match std::str::from_utf8(field) {
        Ok(value) => Cow::Borrowed(value),
        Err(_) => {
            warnings.push(format!("{name} field is not valid UTF-8; decoded lossily"));
            String::from_utf8_lossy(field)
        }
    }
}

/// Trailer ids degrade per value: bad values warn, good ones still link.
fn trailer_ids<T>(values: Vec<&str>, key: &str, warnings: &mut Vec<String>) -> BTreeSet<T>
where
    T: FromStr<Err = anyhow::Error> + Ord,
{
    let mut ids = BTreeSet::new();
    let mut malformed = 0usize;
    let mut first_malformed: Option<anyhow::Error> = None;
    for value in values {
        match value.parse::<T>() {
            Ok(id) => {
                ids.insert(id);
            }
            Err(error) => {
                malformed += 1;
                first_malformed.get_or_insert(error);
            }
        }
    }
    if let Some(error) = first_malformed {
        warnings.push(format!(
            "skipped {malformed} malformed {key}: value(s): {error:#}"
        ));
    }
    ids
}

fn bounded_git_metadata(value: &str, limit: usize) -> String {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn trailer_values<'a>(field: &'a str, key: &str) -> Result<Vec<&'a str>> {
    if field.is_empty() {
        return Ok(Vec::new());
    }
    field
        .split('\u{1d}')
        .map(|trailer| {
            let (actual, value) = trailer
                .split_once(':')
                .context("board_scan: malformed Git trailer field")?;
            ensure!(
                actual.eq_ignore_ascii_case(key),
                "board_scan: unexpected Git trailer key"
            );
            Ok(value.trim())
        })
        .collect()
}

pub(crate) fn shortstat(value: &str) -> Result<(u64, u64, u64)> {
    let value = value.trim();
    if value.is_empty() {
        return Ok((0, 0, 0));
    }
    let mut files = 0;
    let mut insertions = 0;
    let mut deletions = 0;
    for part in value.split(',') {
        let (count, label) = part
            .trim()
            .split_once(' ')
            .context("board_scan: invalid shortstat")?;
        let count = count
            .parse()
            .context("board_scan: invalid shortstat count")?;
        ensure!(
            count <= crate::board::board_ids::MAX_BOARD_NUMBER,
            "board_scan: shortstat count exceeds storage limit"
        );
        match label {
            "file changed" | "files changed" => files = count,
            "insertion(+)" | "insertions(+)" => insertions = count,
            "deletion(-)" | "deletions(-)" => deletions = count,
            _ => anyhow::bail!("board_scan: unknown shortstat field"),
        }
    }
    Ok((files, insertions, deletions))
}
