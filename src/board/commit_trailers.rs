//! Git-native trailer fields and complete, bounded log records.
use super::board_actor::HarnessLabel;
use super::board_ids::{PlanId, RepoKey, TaskId};
use super::board_protocol::{CommitCoauthor, CommitPlanLink, LinkedCommit};
use crate::identity::GitOid;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;

pub type CoauthorLabel = CommitCoauthor;

// Separate keys preserve both trailer presence and each field's meaning. Git's
// `key=` is case-insensitive; a pipe-separated key is one literal key.
pub const LOG_FORMAT: &str = "--format=%x00%H%x00%ct%x00%an <%ae>%x00%s%x00%(trailers:key=Plan,unfold,separator=%x1d)%x00%(trailers:key=Plan-Task,unfold,separator=%x1d)%x00%(trailers:key=Co-authored-by,unfold,separator=%x1d)%x00";

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
            Ok(commit) => result.records.push(commit),
            Err(error) => result.warnings.push(format!(
                "board_scan: skipped Git record {}: {error:#}",
                index + 1
            )),
        }
    }
    Ok(result)
}

fn parse_record(fields: &[&[u8]], repo_key: &RepoKey) -> Result<ParsedCommit> {
    let text: Vec<_> = fields
        .iter()
        .map(|field| std::str::from_utf8(field))
        .collect::<std::result::Result<_, _>>()?;
    let plans = trailer_values(text[4], "Plan")?;
    let tasks = trailer_values(text[5], "Plan-Task")?
        .into_iter()
        .map(str::parse::<TaskId>)
        .collect::<Result<BTreeSet<_>>>()?;
    let mut links = Vec::with_capacity(plans.len() + tasks.len());
    for plan in plans
        .iter()
        .map(|value| value.parse::<PlanId>())
        .collect::<Result<BTreeSet<_>>>()?
    {
        let before = links.len();
        for task in tasks.iter().filter(|task| task.plan == plan) {
            links.push(CommitPlanLink {
                plan_id: plan,
                task_ordinal: Some(task.ordinal),
            });
        }
        if links.len() == before {
            links.push(CommitPlanLink {
                plan_id: plan,
                task_ordinal: None,
            });
        }
    }
    ensure!(links.len() <= 256, "board_scan: too many plan references");
    let mut coauthors = Vec::new();
    for value in trailer_values(text[6], "Co-authored-by")? {
        let coauthor = parse_coauthor(value)?;
        if !coauthors.contains(&coauthor) {
            coauthors.push(coauthor);
        }
    }
    ensure!(coauthors.len() <= 64, "board_scan: too many coauthors");
    let (files, insertions, deletions) = shortstat(text[7]).unwrap_or_default();
    Ok(ParsedCommit {
        has_plan_trailer: !plans.is_empty(),
        commit: LinkedCommit {
            repo_key: repo_key.clone(),
            oid: GitOid::parse(text[0])?,
            committed_at: text[1]
                .parse()
                .context("board_scan: invalid commit timestamp")?,
            author: bounded_git_metadata(text[2], 1024),
            subject: bounded_git_metadata(text[3], 1024),
            coauthors,
            files,
            insertions,
            deletions,
            plans: links,
        },
    })
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
            count <= super::board_ids::MAX_BOARD_NUMBER,
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
        let malformed = format!("\0{oid}\01700000000\0Fixture <f@example.test>\0");
        bytes.extend_from_slice(malformed.as_bytes());
        bytes.extend_from_slice(b"\xff\0Plan: P7\0\0\0\n");
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
}
