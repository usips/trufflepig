//! SSOT body diffs between plan revisions, with bounded context lines.

use crate::board::board_ids::PlanRevision;
use crate::history::source_diff::diff_sources;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SsotDiff {
    pub before: PlanRevision,
    pub after: PlanRevision,
    pub hunks: Vec<SsotHunk>,
    pub next: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SsotHunk {
    pub before_start: usize,
    pub after_start: usize,
    pub removed: Vec<String>,
    pub added: Vec<String>,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

pub fn build_ssot_diff(
    before: &crate::board::board_protocol::RevisionRecord,
    after: &crate::board::board_protocol::RevisionRecord,
) -> SsotDiff {
    let before_text = before.body.as_str();
    let after_text = after.body.as_str();
    let changes = diff_sources(before_text.as_bytes(), after_text.as_bytes());
    let before_lines = before_text.split_inclusive('\n').collect::<Vec<_>>();
    let after_lines = after_text.split_inclusive('\n').collect::<Vec<_>>();
    let hunks = changes
        .changes
        .iter()
        .enumerate()
        .map(|(index, change)| {
            let start = change.before_lines.start;
            let end = change.before_lines.end;
            let context_start = start.saturating_sub(3).max(
                index
                    .checked_sub(1)
                    .map_or(0, |previous| changes.changes[previous].before_lines.end),
            );
            let context_end = (end + 3).min(
                changes
                    .changes
                    .get(index + 1)
                    .map_or(before_lines.len(), |next| next.before_lines.start),
            );
            SsotHunk {
                before_start: start + 1,
                after_start: change.after_lines.start + 1,
                removed: before_lines[change.before_lines.clone()]
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect(),
                added: after_lines[change.after_lines.clone()]
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect(),
                context_before: before_lines[context_start..start]
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect(),
                context_after: before_lines[end..context_end]
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect(),
            }
        })
        .collect();
    SsotDiff {
        before: before.id,
        after: after.id,
        hunks,
        next: None,
    }
}
