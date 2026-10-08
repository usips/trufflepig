//! Git-native trailer fields and complete, bounded log records.
#[cfg(test)]
mod tests;
mod trailer_records;

use crate::board::board_actor::{AgentVendor, HarnessLabel};
use crate::board::board_ids::RepoKey;
use crate::board::board_protocol::{CommitCoauthor, LinkedCommit};
use anyhow::{Context, Result, ensure};
use trailer_records::parse_record;
pub(crate) use trailer_records::shortstat;

// Separate keys preserve both trailer presence and each field's meaning. Git's
// `key=` is case-insensitive; a pipe-separated key is one literal key. Bodies
// stay out of the format: a bounded window of long bodies must not overflow
// the output limit, so misplaced trailers are found by a second grep pass.
pub const LOG_FORMAT: &str = concat!(
    "--format=%x00%H%x00%ct%x00%an <%ae>%x00%s%x00",
    "%(trailers:key=Plan,unfold,separator=%x1d)%x00",
    "%(trailers:key=Plan-Task,unfold,separator=%x1d)%x00",
    "%(trailers:key=Co-authored-by,unfold,separator=%x1d)%x00"
);

const RECORD_FIELDS: usize = 8;

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
/// Warnings key by commit oid and deduplicate, so repeated scans name an oid once.
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
    for record in fields.chunks_exact(RECORD_FIELDS) {
        let oid = String::from_utf8_lossy(record[0]).into_owned();
        match parse_record(record, repo_key) {
            Ok(parsed) => {
                result.warnings.extend(
                    parsed
                        .warnings
                        .into_iter()
                        .map(|warning| format!("board_scan: {oid}: {warning}")),
                );
                result.records.push(parsed.commit);
            }
            Err(error) => result
                .warnings
                .push(format!("board_scan: {oid}: skipped record: {error:#}")),
        }
    }
    let mut seen = std::collections::HashSet::with_capacity(result.warnings.len());
    result
        .warnings
        .retain(|warning| seen.insert(warning.clone()));
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
    let vendor = match domain.to_ascii_lowercase().as_str() {
        "anthropic.com" => AgentVendor::Claude,
        "openai.com" => AgentVendor::Codex,
        "moonshot.ai" => AgentVendor::Kimi,
        "x.ai" => AgentVendor::Grok,
        "google.com" => AgentVendor::Gemini,
        "qwen.ai" => AgentVendor::Qwen,
        "meta.com" | "muse.ai" => AgentVendor::Muse,
        _ => AgentVendor::Unknown,
    };
    let harness = if vendor == AgentVendor::Unknown {
        HarnessLabel::parse(&format!("git:{email}"))?
    } else {
        HarnessLabel::parse(vendor.as_str())?
    };
    Ok(CommitCoauthor {
        harness,
        model: model.to_owned(),
        email: email.to_owned(),
    })
}

/// Unknown vendors retain the claim's exact harness attribution identity.
pub(crate) fn coauthors_match_claim(
    coauthors: &[CommitCoauthor],
    vendor: AgentVendor,
    claimed_harness: &HarnessLabel,
) -> bool {
    if coauthors.is_empty() {
        return vendor == AgentVendor::Human;
    }
    let label = if vendor == AgentVendor::Unknown {
        claimed_harness.as_str()
    } else {
        vendor.as_str()
    };
    coauthors
        .iter()
        .any(|coauthor| coauthor.harness.as_str() == label)
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
