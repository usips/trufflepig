//! Declaration correspondence preserves occurrence indexes and incomplete extraction evidence.

use super::{SourceDiff, line_offsets};
use crate::extract::{Definition, Extraction};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationRelation {
    Unchanged,
    Modified,
    Moved,
    Added,
    Deleted,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclarationCorrespondence {
    pub before: Option<usize>,
    pub after: Option<usize>,
    pub candidates: Vec<usize>,
    pub candidates_truncated: bool,
    pub relation: DeclarationRelation,
}

type DeclarationKey<'a> = (&'a str, &'a str, Option<&'a str>);

fn key(definition: &Definition) -> DeclarationKey<'_> {
    (
        &definition.name,
        &definition.kind,
        definition.container.as_deref(),
    )
}

fn group_definitions(extraction: &Extraction) -> HashMap<DeclarationKey<'_>, Vec<usize>> {
    let mut groups = HashMap::with_capacity(extraction.definitions.len());
    for (index, definition) in extraction.definitions.iter().enumerate() {
        groups
            .entry(key(definition))
            .or_insert_with(Vec::new)
            .push(index);
    }
    groups
}

fn position(offsets: &[usize], start: usize) -> (usize, usize) {
    let line = offsets
        .partition_point(|&offset| offset <= start)
        .saturating_sub(1);
    (line, start - offsets[line])
}

fn content<'a>(source: &'a [u8], definition: &Definition) -> Option<&'a [u8]> {
    source
        .get(definition.start..definition.end)
        .filter(|bytes| !bytes.is_empty())
}

/// Rows refer to extraction definition indexes, including duplicate declarations.
/// Missing declarations are definitive only when the opposite extraction is complete.
pub fn correspond_declarations(
    before: &[u8],
    after: &[u8],
    before_extraction: &Extraction,
    after_extraction: &Extraction,
    diff: &SourceDiff,
) -> Vec<DeclarationCorrespondence> {
    let before_groups = group_definitions(before_extraction);
    let after_groups = group_definitions(after_extraction);
    let before_offsets = line_offsets(before);
    let after_offsets = line_offsets(after);
    let mut matches = vec![None; before_extraction.definitions.len()];
    let mut used_after = vec![false; after_extraction.definitions.len()];
    // Exact mapped positions disambiguate repeated names without collapsing occurrences.
    let mut after_positions = HashMap::with_capacity(after_extraction.definitions.len());
    for (index, definition) in after_extraction.definitions.iter().enumerate() {
        after_positions
            .entry((key(definition), position(&after_offsets, definition.start)))
            .or_insert_with(Vec::new)
            .push(index);
    }
    for (index, definition) in before_extraction.definitions.iter().enumerate() {
        let (line, column) = position(&before_offsets, definition.start);
        let Some(mapped) = diff.map_before_line(line) else {
            continue;
        };
        let Some(candidates) = after_positions.get(&(key(definition), (mapped, column))) else {
            continue;
        };
        if let [candidate] = candidates.as_slice() {
            if !used_after[*candidate] {
                let equal = content(before, definition)
                    .zip(content(after, &after_extraction.definitions[*candidate]))
                    .is_some_and(|(left, right)| left == right);
                matches[index] = Some((
                    *candidate,
                    if equal {
                        DeclarationRelation::Unchanged
                    } else {
                        DeclarationRelation::Modified
                    },
                ));
                used_after[*candidate] = true;
            }
        }
    }
    for (declaration_key, before_indexes) in &before_groups {
        let Some(after_indexes) = after_groups.get(declaration_key) else {
            continue;
        };
        if let ([left], [right]) = (before_indexes.as_slice(), after_indexes.as_slice()) {
            if matches[*left].is_none() && !used_after[*right] {
                let before_line =
                    position(&before_offsets, before_extraction.definitions[*left].start).0;
                let after_line =
                    position(&after_offsets, after_extraction.definitions[*right].start).0;
                if diff.shares_change(before_line, after_line) {
                    matches[*left] = Some((*right, DeclarationRelation::Modified));
                    used_after[*right] = true;
                }
            }
        }
        // A move requires exact content unique on both sides, even among matched rows.
        let mut before_content = HashMap::with_capacity(before_indexes.len());
        let mut after_content = HashMap::with_capacity(after_indexes.len());
        for &index in before_indexes {
            if let Some(bytes) = content(before, &before_extraction.definitions[index]) {
                before_content
                    .entry(bytes)
                    .or_insert_with(Vec::new)
                    .push(index);
            }
        }
        for &index in after_indexes {
            if let Some(bytes) = content(after, &after_extraction.definitions[index]) {
                after_content
                    .entry(bytes)
                    .or_insert_with(Vec::new)
                    .push(index);
            }
        }
        for (bytes, lefts) in before_content {
            let Some(rights) = after_content.get(bytes) else {
                continue;
            };
            if let ([left], [right]) = (lefts.as_slice(), rights.as_slice()) {
                if matches[*left].is_none() && !used_after[*right] {
                    matches[*left] = Some((*right, DeclarationRelation::Moved));
                    used_after[*right] = true;
                }
            }
        }
    }
    let mut rows = Vec::with_capacity(
        before_extraction.definitions.len() + after_extraction.definitions.len(),
    );
    let mut uncertain_after = vec![false; after_extraction.definitions.len()];
    let mut remaining_groups = HashMap::with_capacity(after_groups.len());
    for (declaration_key, after_indexes) in &after_groups {
        let uncertain = before_groups
            .get(declaration_key)
            .is_some_and(|indexes| indexes.iter().any(|&index| matches[index].is_none()));
        let mut candidates = Vec::with_capacity(after_indexes.len().min(64));
        let mut count = 0;
        for &index in after_indexes {
            if !used_after[index] {
                uncertain_after[index] = uncertain;
                count += 1;
                if candidates.len() < 64 {
                    candidates.push(index);
                }
            }
        }
        remaining_groups.insert(*declaration_key, (candidates, count));
    }
    let mut remaining_candidate_links = 4096;
    for (index, definition) in before_extraction.definitions.iter().enumerate() {
        if let Some((after, relation)) = matches[index] {
            rows.push(DeclarationCorrespondence {
                before: Some(index),
                after: Some(after),
                candidates: Vec::new(),
                candidates_truncated: false,
                relation,
            });
            continue;
        }
        let remaining = remaining_groups
            .get(&key(definition))
            .map(|(candidates, count)| (candidates.as_slice(), *count))
            .unwrap_or((&[], 0));
        let candidates = remaining.0[..remaining.0.len().min(remaining_candidate_links)].to_vec();
        remaining_candidate_links -= candidates.len();
        let candidates_truncated = candidates.len() < remaining.1;
        let relation = if remaining.1 == 0
            && before_extraction.status == "complete"
            && after_extraction.status == "complete"
        {
            DeclarationRelation::Deleted
        } else {
            DeclarationRelation::Uncertain
        };
        rows.push(DeclarationCorrespondence {
            before: Some(index),
            after: None,
            candidates,
            candidates_truncated,
            relation,
        });
    }
    for (index, &used) in used_after.iter().enumerate() {
        if !used {
            let relation = if before_extraction.status == "complete"
                && after_extraction.status == "complete"
                && !uncertain_after[index]
            {
                DeclarationRelation::Added
            } else {
                DeclarationRelation::Uncertain
            };
            rows.push(DeclarationCorrespondence {
                before: None,
                after: Some(index),
                candidates: Vec::new(),
                candidates_truncated: false,
                relation,
            });
        }
    }
    rows
}
