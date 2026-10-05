//! Git-native trailer fields and complete, bounded log records.
use crate::board::board_actor::HarnessLabel;
use crate::board::board_ids::{PlanId, RepoKey, TaskId};
use crate::board::board_protocol::{
    COAUTHOR_LIMIT, CommitCoauthor, CommitPlanLink, LINK_LIMIT, LinkedCommit,
};
use crate::identity::GitOid;
use anyhow::{Context, Result, ensure};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::str::FromStr;

// Separate keys preserve both trailer presence and each field's meaning. Git's
// `key=` is case-insensitive; a pipe-separated key is one literal key.
pub const LOG_FORMAT: &str = concat!(
    "--format=%x00%H%x00%ct%x00%an <%ae>%x00%s%x00",
    "%(trailers:key=Plan,unfold,separator=%x1d)%x00",
    "%(trailers:key=Plan-Task,unfold,separator=%x1d)%x00",
    "%(trailers:key=Co-authored-by,unfold,separator=%x1d)%x00"
);

#[derive(Clone, Debug)]
pub struct ParsedCommit {
    pub commit: LinkedCommit,
    pub has_plan_trailer: bool,
}

pub struct ParsedLog {
    pub records: Vec<ParsedCommit>,
    pub record_count: usize,
    pub warnings: Vec<String>,
}

/// Framing is atomic; invalid metadata skips one record without discarding its peers.
/// Plan, task, and co-author trailers degrade per value: bad entries warn only.
pub fn parse_log(bytes: &[u8], repo_key: &RepoKey) -> Result<ParsedLog> {
    if bytes.is_empty() {
        return Ok(ParsedLog {
            records: Vec::new(),
            record_count: 0,
            warnings: Vec::new(),
        });
    }
    ensure!(bytes[0] == 0, "board_scan: invalid Git log framing");
    let fields: Vec<_> = bytes[1..].split(|byte| *byte == 0).collect();
    ensure!(
        fields.len() % 8 == 0,
        "board_scan: incomplete Git log record"
    );
    let mut result = ParsedLog {
        records: Vec::with_capacity(fields.len() / 8),
        record_count: fields.len() / 8,
        warnings: Vec::new(),
    };
    for (index, record) in fields.chunks_exact(8).enumerate() {
        match parse_record(record, repo_key) {
            Ok(parsed) => {
                result
                    .warnings
                    .extend(parsed.warnings.into_iter().map(|warning| {
                        format!("board_scan: Git record {}: {warning}", index + 1)
                    }));
                result.records.push(parsed.commit);
            }
            Err(error) => result.warnings.push(format!(
                "board_scan: skipped Git record {}: {error:#}",
                index + 1
            )),
        }
    }
    Ok(result)
}

struct ParsedRecord {
    commit: ParsedCommit,
    warnings: Vec<String>,
}

fn parse_record(fields: &[&[u8]], repo_key: &RepoKey) -> Result<ParsedRecord> {
    let mut warnings = Vec::new();
    let oid = std::str::from_utf8(fields[0])?;
    let timestamp = std::str::from_utf8(fields[1])?;
    let author = lossy_field(fields[2], "author", &mut warnings);
    let subject = lossy_field(fields[3], "subject", &mut warnings);
    let plan_field = std::str::from_utf8(fields[4])?;
    let task_field = std::str::from_utf8(fields[5])?;
    let coauthor_field = std::str::from_utf8(fields[6])?;
    let shortstat_field = std::str::from_utf8(fields[7])?;
    let raw_plans = trailer_values(plan_field, "Plan")?;
    let has_plan_trailer = !raw_plans.is_empty();
    let plans: BTreeSet<PlanId> = trailer_ids(raw_plans, "Plan", &mut warnings);
    let tasks: BTreeSet<TaskId> = trailer_ids(
        trailer_values(task_field, "Plan-Task")?,
        "Plan-Task",
        &mut warnings,
    );
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
    for value in trailer_values(coauthor_field, "Co-authored-by")? {
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
    let (files, insertions, deletions) = shortstat(shortstat_field).unwrap_or_default();
    Ok(ParsedRecord {
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

/// Author and subject decode lossily so one bad byte never drops a record.
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

pub fn parse_coauthor(value: &str) -> Result<CommitCoauthor> {
    ensure!(
        !value.chars().any(char::is_control),
        "board_scan: control character in coauthor"
    );
    let (model, email) = value
        .rsplit_once('<')
        .context("board_scan: coauthor requires Name <email>")?;
    let model = model.trim();
    let email = email
        .strip_suffix('>')
        .context("board_scan: coauthor requires closing >")?
        .trim();
    ensure!(
        !model.is_empty()
            && model.len() <= 256
            && !email.is_empty()
            && email.len() <= 256
            && !email.chars().any(char::is_whitespace),
        "board_scan: invalid coauthor"
    );
    let (local, domain) = email
        .rsplit_once('@')
        .context("board_scan: coauthor requires email domain")?;
    ensure!(
        !local.is_empty() && !domain.is_empty(),
        "board_scan: invalid coauthor email"
    );
    let label = match domain.to_ascii_lowercase().as_str() {
        "anthropic.com" => "claude",
        "openai.com" => "codex",
        "moonshot.ai" => "kimi",
        "x.ai" => "grok",
        "google.com" => "gemini",
        "qwen.ai" => "qwen",
        _ => "",
    };
    let harness = if label.is_empty() {
        HarnessLabel::parse(&format!("git:{email}"))?
    } else {
        HarnessLabel::parse(label)?
    };
    Ok(CommitCoauthor {
        harness,
        model: model.to_owned(),
        email: email.to_owned(),
    })
}

pub fn attributed_to(commit: &LinkedCommit, agent: &HarnessLabel) -> bool {
    if commit.coauthors.is_empty() {
        agent.as_str() == "human"
    } else {
        commit
            .coauthors
            .iter()
            .any(|coauthor| &coauthor.harness == agent)
    }
}

fn shortstat(value: &str) -> Result<(u64, u64, u64)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_attribution_uses_email_domain_and_retains_model_claim() {
        for (domain, label) in [
            ("Anthropic.com", "claude"),
            ("openai.com", "codex"),
            ("moonshot.ai", "kimi"),
            ("x.ai", "grok"),
            ("google.com", "gemini"),
            ("qwen.ai", "qwen"),
        ] {
            let coauthor = parse_coauthor(&format!("Model claim <agent@{domain}>")).unwrap();
            assert_eq!(coauthor.harness.as_str(), label);
            assert_eq!(coauthor.model, "Model claim");
        }
        assert_eq!(
            parse_coauthor("Human <a@openai.com.evil.test>")
                .unwrap()
                .harness
                .as_str(),
            "git:a@openai.com.evil.test"
        );
    }

    #[test]
    fn invalid_utf8_record_is_skipped_without_losing_raw_count() {
        let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
        let oid = "b".repeat(40);
        let valid =
            format!("\0{oid}\01700000000\0Fixture <f@example.test>\0valid\0Plan: P7\0\0\0\n");
        let mut bytes = valid.as_bytes().to_vec();
        let malformed = format!("\0{oid}\01700000000\0Fixture <f@example.test>\0valid\0");
        bytes.extend_from_slice(malformed.as_bytes());
        bytes.extend_from_slice(b"\xff\0\0\0\n");
        bytes.extend_from_slice(valid.as_bytes());
        let parsed = parse_log(&bytes, &key).unwrap();
        assert_eq!(parsed.record_count, 3);
        assert_eq!(parsed.records.len(), 2);
        assert_eq!(parsed.warnings.len(), 1);
    }

    #[test]
    fn incomplete_framing_is_rejected_without_partial_records() {
        let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
        assert!(parse_log(b"\0bad\0record", &key).is_err());
        assert!(parse_coauthor("Model <email>").is_err());
    }

    #[test]
    fn malformed_plan_task_value_warns_without_dropping_valid_link() {
        let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
        let oid = "b".repeat(40);
        let bytes = format!(
            "\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0Plan: P7\0Plan-Task: P7.1\x1dPlan-Task: garbage\0\0\n"
        );
        let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(
            parsed.records[0].commit.plans,
            vec![CommitPlanLink {
                plan_id: PlanId::new(7).unwrap(),
                task_ordinal: Some(1),
            }]
        );
        assert_eq!(parsed.warnings.len(), 1);
        assert!(
            parsed.warnings[0].contains("malformed Plan-Task"),
            "unexpected warning: {}",
            parsed.warnings[0]
        );
    }

    #[test]
    fn malformed_plan_value_warns_without_dropping_valid_link() {
        let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
        let oid = "b".repeat(40);
        let bytes = format!(
            "\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0Plan: P7\x1dPlan: garbage\0\0\0\n"
        );
        let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(
            parsed.records[0].commit.plans,
            vec![CommitPlanLink {
                plan_id: PlanId::new(7).unwrap(),
                task_ordinal: None,
            }]
        );
        assert_eq!(parsed.warnings.len(), 1);
        assert!(
            parsed.warnings[0].contains("malformed Plan:"),
            "unexpected warning: {}",
            parsed.warnings[0]
        );
    }

    #[test]
    fn all_bad_plan_values_still_record_the_commit() {
        let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
        let oid = "b".repeat(40);
        let bytes = format!(
            "\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0Plan: garbage\0\0\0\n"
        );
        let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
        assert_eq!(parsed.records.len(), 1);
        assert!(parsed.records[0].commit.plans.is_empty());
        assert!(parsed.records[0].has_plan_trailer);
        assert_eq!(parsed.warnings.len(), 1);
    }

    #[test]
    fn latin1_author_decodes_lossily_with_warning() {
        let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
        let oid = "b".repeat(40);
        let mut bytes = format!("\0{oid}\01700000000\0").into_bytes();
        bytes.extend_from_slice(b"Caf\xe9 <c@example.test>");
        bytes.extend_from_slice(b"\0subject\0Plan: P7\0\0\0\n");
        let parsed = parse_log(&bytes, &key).unwrap();
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(
            parsed.records[0].commit.author,
            "Caf\u{fffd} <c@example.test>"
        );
        assert!(!parsed.records[0].commit.plans.is_empty());
        assert_eq!(parsed.warnings.len(), 1);
        assert!(
            parsed.warnings[0].contains("author"),
            "unexpected warning: {}",
            parsed.warnings[0]
        );
    }

    #[test]
    fn plan_links_past_limit_truncate_with_warning() {
        let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
        let oid = "b".repeat(40);
        let plans = (1..=260u64)
            .map(|number| format!("Plan: P{number}"))
            .collect::<Vec<_>>()
            .join("\x1d");
        let bytes =
            format!("\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0{plans}\0\0\0\n");
        let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
        assert_eq!(parsed.records.len(), 1);
        let links = &parsed.records[0].commit.plans;
        assert_eq!(links.len(), 256);
        assert_eq!(links.first().unwrap().plan_id, PlanId::new(1).unwrap());
        assert_eq!(links.last().unwrap().plan_id, PlanId::new(256).unwrap());
        assert_eq!(parsed.warnings.len(), 1);
        assert!(
            parsed.warnings[0].contains("past the 256 limit"),
            "unexpected warning: {}",
            parsed.warnings[0]
        );
    }
}
