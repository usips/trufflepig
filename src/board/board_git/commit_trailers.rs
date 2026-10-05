//! Git-native trailer fields and complete, bounded log records.
#[cfg(test)]
mod tests;
mod trailer_records;

use crate::board::board_actor::HarnessLabel;
use crate::board::board_ids::RepoKey;
use crate::board::board_protocol::{CommitCoauthor, LinkedCommit};
use anyhow::{Context, Result, ensure};
use trailer_records::parse_record;

// Separate keys preserve both trailer presence and each field's meaning. Git's
// `key=` is case-insensitive; a pipe-separated key is one literal key. The raw
// body detects trailer-shaped lines Git's final-paragraph rule ignored.
pub const LOG_FORMAT: &str = concat!(
    "--format=%x00%H%x00%ct%x00%an <%ae>%x00%s%x00%B%x00",
    "%(trailers:key=Plan,unfold,separator=%x1d)%x00",
    "%(trailers:key=Plan-Task,unfold,separator=%x1d)%x00",
    "%(trailers:key=Co-authored-by,unfold,separator=%x1d)%x00"
);

const RECORD_FIELDS: usize = 9;

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
/// Body lines that look like plan trailers but were not parsed warn per commit.
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
        fields.len() % RECORD_FIELDS == 0,
        "board_scan: incomplete Git log record"
    );
    let mut result = ParsedLog {
        records: Vec::with_capacity(fields.len() / RECORD_FIELDS),
        record_count: fields.len() / RECORD_FIELDS,
        warnings: Vec::new(),
    };
    for (index, record) in fields.chunks_exact(RECORD_FIELDS).enumerate() {
        match parse_record(record, repo_key) {
            Ok(parsed) => {
                result.warnings.extend(
                    parsed
                        .warnings
                        .into_iter()
                        .map(|warning| format!("board_scan: Git record {}: {warning}", index + 1)),
                );
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
        "meta.com" => "muse",
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
