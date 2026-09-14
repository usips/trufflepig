//! Declaration correspondence preserves occurrence indexes and incomplete extraction evidence.

use super::{SourceDiff, line_offsets};
use crate::extract::{Definition, Extraction};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, hash::Hash};

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

pub(super) struct MatchFact<'a, K, C: ?Sized> {
    pub key: K,
    pub start: usize,
    pub content: Option<&'a C>,
}

fn group_definitions<K: Copy + Eq + Hash, C: ?Sized>(
    facts: &[MatchFact<'_, K, C>],
) -> HashMap<K, Vec<usize>> {
    let mut groups = HashMap::with_capacity(facts.len());
    for (index, fact) in facts.iter().enumerate() {
        groups.entry(fact.key).or_insert_with(Vec::new).push(index);
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
/// Additions and deletions are definitive only when both extractions are complete.
pub fn correspond_declarations(
    before: &[u8],
    after: &[u8],
    before_extraction: &Extraction,
    after_extraction: &Extraction,
    diff: &SourceDiff,
) -> Vec<DeclarationCorrespondence> {
    let before_facts: Vec<_> = before_extraction
        .definitions
        .iter()
        .map(|definition| MatchFact {
            key: key(definition),
            start: definition.start,
            content: content(before, definition),
        })
        .collect();
    let after_facts: Vec<_> = after_extraction
        .definitions
        .iter()
        .map(|definition| MatchFact {
            key: key(definition),
            start: definition.start,
            content: content(after, definition),
        })
        .collect();
    match_facts(
        &before_facts,
        &after_facts,
        &line_offsets(before),
        &line_offsets(after),
        before_extraction.status == "complete" && after_extraction.status == "complete",
        diff,
    )
}

pub(super) fn match_facts<K: Copy + Eq + Hash, C: ?Sized + Eq + Hash>(
    before: &[MatchFact<'_, K, C>],
    after: &[MatchFact<'_, K, C>],
    before_offsets: &[usize],
    after_offsets: &[usize],
    complete: bool,
    diff: &SourceDiff,
) -> Vec<DeclarationCorrespondence> {
    let before_groups = group_definitions(before);
    let after_groups = group_definitions(after);
    let mut matches = vec![None; before.len()];
    let mut used_after = vec![false; after.len()];
    // Exact mapped positions disambiguate repeated names without collapsing occurrences.
    let mut before_positions = HashMap::with_capacity(before.len());
    for fact in before {
        *before_positions
            .entry((fact.key, position(before_offsets, fact.start)))
            .or_insert(0usize) += 1;
    }
    let mut after_positions = HashMap::with_capacity(after.len());
    for (index, definition) in after.iter().enumerate() {
        after_positions
            .entry((definition.key, position(after_offsets, definition.start)))
            .or_insert_with(Vec::new)
            .push(index);
    }
    for (index, definition) in before.iter().enumerate() {
        let (line, column) = position(before_offsets, definition.start);
        if before_positions.get(&(definition.key, (line, column))) != Some(&1) {
            continue;
        }
        let Some(mapped) = diff.map_before_line(line) else {
            continue;
        };
        let Some(candidates) = after_positions.get(&(definition.key, (mapped, column))) else {
            continue;
        };
        if let [candidate] = candidates.as_slice() {
            if !used_after[*candidate] {
                let equal = definition
                    .content
                    .zip(after[*candidate].content)
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
                let before_line = position(before_offsets, before[*left].start).0;
                let after_line = position(after_offsets, after[*right].start).0;
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
            if let Some(bytes) = before[index].content {
                before_content
                    .entry(bytes)
                    .or_insert_with(Vec::new)
                    .push(index);
            }
        }
        for &index in after_indexes {
            if let Some(bytes) = after[index].content {
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
    let mut rows = Vec::with_capacity(before.len() + after.len());
    let mut uncertain_after = vec![false; after.len()];
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
    for (index, definition) in before.iter().enumerate() {
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
            .get(&definition.key)
            .map(|(candidates, count)| (candidates.as_slice(), *count))
            .unwrap_or((&[], 0));
        let candidates = remaining.0[..remaining.0.len().min(remaining_candidate_links)].to_vec();
        remaining_candidate_links -= candidates.len();
        let candidates_truncated = candidates.len() < remaining.1;
        let relation = if remaining.1 == 0 && complete {
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
            let relation = if complete && !uncertain_after[index] {
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
