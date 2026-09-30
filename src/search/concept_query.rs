//! Free-text query shapes and the lexical lanes they run. A literal `"…"`
//! query runs only the phrase lane. Other text runs, in fusion order: exact
//! definitions (for `A::b`, lane D's qualified definitions), identifier
//! occurrences (one identifier-shaped token), phrase and all-terms lanes (two
//! or more terms), the any-term lane, and filenames.
use super::{
    BoundPathFilter, LaneHits, Query, declarations, file_hits, file_ranking, hit_row, lexical_hits,
    qualified_name::QualifiedName,
    telemetry::{Lane, LaneOutcome, RetrievalTrace},
};
use crate::{results::Hit, store::Store};
use anyhow::Result;
use rusqlite::params;

#[cfg(test)]
mod tests;

/// FTS5 expressions and token shape derived from one free-text query.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ConceptQuery {
    /// Any term; absent for literals and term-free text.
    pub any_terms: Option<String>,
    /// Every term, adjacent and in order; literals and two or more terms.
    pub phrase: Option<String>,
    /// Every term within one region; two or more free-text terms.
    pub all_terms: Option<String>,
    pub identifier: Option<IdentifierToken>,
    /// The text was one `"…"` literal.
    pub literal: bool,
}

/// A single token shaped like code: it has `_`, `::`, a lower-to-upper case
/// change, or letters mixed with digits. `a::b::c` names `c` in `b`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct IdentifierToken {
    pub name: String,
    pub qualifier: Option<String>,
}

impl ConceptQuery {
    pub(super) fn parse(text: &str) -> Self {
        let text = text.trim();
        let literal = text.len() >= 2
            && text.starts_with('"')
            && text.ends_with('"')
            && !text[1..text.len() - 1].contains('"');
        let body = if literal {
            &text[1..text.len() - 1]
        } else {
            text
        };
        let terms: Vec<&str> = body
            .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
            .filter(|term| !term.is_empty())
            .collect();
        if terms.is_empty() {
            return Self {
                literal,
                ..Self::default()
            };
        }
        let quoted: Vec<String> = terms.iter().map(|term| format!("\"{term}\"")).collect();
        let phrase = (literal || terms.len() > 1).then(|| format!("\"{}\"", terms.join(" ")));
        if literal {
            return Self {
                phrase,
                literal,
                ..Self::default()
            };
        }
        Self {
            any_terms: Some(super::fts_terms(body)),
            all_terms: (terms.len() > 1).then(|| quoted.join(" AND ")),
            phrase,
            identifier: IdentifierToken::parse(text),
            literal,
        }
    }
}

impl IdentifierToken {
    fn parse(text: &str) -> Option<Self> {
        let segments: Vec<&str> = text.split("::").collect();
        let identifier_segment = |segment: &&str| {
            !segment.is_empty() && segment.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
        };
        if !segments.iter().all(identifier_segment) || !text.chars().any(char::is_alphabetic) {
            return None;
        }
        let chars: Vec<char> = text.chars().collect();
        let camel = chars
            .windows(2)
            .any(|pair| pair[0].is_lowercase() && pair[1].is_uppercase());
        let digits = chars.iter().any(char::is_ascii_digit);
        if segments.len() == 1 && !text.contains('_') && !camel && !digits {
            return None;
        }
        Some(Self {
            name: segments[segments.len() - 1].to_owned(),
            qualifier: (segments.len() > 1).then(|| segments[segments.len() - 2].to_owned()),
        })
    }
}

/// Runs the lexical lanes after `exact` in fusion order; returns them with
/// whether any lane reached its candidate cap.
pub(super) fn concept_lanes(
    store: &Store,
    query: &Query,
    paths: &BoundPathFilter,
    concept: &ConceptQuery,
    exact: LaneHits,
    trace: &mut RetrievalTrace,
) -> Result<(Vec<Vec<Hit>>, bool)> {
    let mut truncated = exact.truncated;
    let mut lanes = Vec::with_capacity(6);
    lanes.push(exact.hits);
    let mut add = |lane: Result<LaneHits>| -> Result<()> {
        let lane = lane?;
        truncated |= lane.truncated;
        lanes.push(lane.hits);
        Ok(())
    };
    if let Some(identifier) = &concept.identifier {
        if identifier.qualifier.is_some() {
            let name = QualifiedName::parse(&query.text);
            add(recorded(trace, Lane::ExactIdentifier, || {
                declarations::qualified_hits(store, &name, paths, &query.language, &query.kind)
            }))?;
        }
        add(recorded(trace, Lane::IdentifierOccurrence, || {
            occurrence_hits(store, query, paths, identifier)
        }))?;
    }
    if let Some(phrase) = &concept.phrase {
        add(recorded(trace, Lane::Phrase, || {
            relabeled(lexical_hits(store, query, paths, phrase), "phrase")
        }))?;
    }
    if let Some(all_terms) = &concept.all_terms {
        add(recorded(trace, Lane::AllTerms, || {
            relabeled(lexical_hits(store, query, paths, all_terms), "all_terms")
        }))?;
    }
    if let Some(any_terms) = &concept.any_terms {
        add(recorded(trace, Lane::Lexical, || {
            lexical_hits(store, query, paths, any_terms)
        }))?;
        if query.kind.is_empty() || query.kind == "file" {
            add(recorded(trace, Lane::File, || {
                file_hits(store, query, paths)
            }))?;
        }
    }
    Ok((lanes, truncated))
}

/// Tags an FTS5 lane's hits with the lane that found them.
fn relabeled(lane: Result<LaneHits>, provenance: &str) -> Result<LaneHits> {
    let mut lane = lane?;
    for hit in &mut lane.hits {
        hit.provenance = Some(provenance.to_owned());
    }
    Ok(lane)
}

/// Records one lane's outcome and candidates in `trace`.
pub(super) fn recorded(
    trace: &mut RetrievalTrace,
    lane: Lane,
    run: impl FnOnce() -> Result<LaneHits>,
) -> Result<LaneHits> {
    let started = std::time::Instant::now();
    let outcome = run();
    trace.record(
        lane,
        started,
        outcome
            .as_ref()
            .map_or(&[][..], |lane| lane.hits.as_slice()),
        LaneOutcome::from_result(&outcome, false),
        outcome.as_ref().is_ok_and(|lane| lane.truncated),
    );
    outcome
}

/// Files with an occurrence named exactly `identifier.name` (and, when
/// qualified, an occurrence of the qualifier), most uses first. Each file is
/// represented by the region around its first declaration or use, imports last.
fn occurrence_hits(
    store: &Store,
    query: &Query,
    paths: &BoundPathFilter,
    identifier: &IdentifierToken,
) -> Result<LaneHits> {
    let mut statement = store.conn.prepare(&format!(
        "WITH matched AS (
             SELECT o.file_id,r.start,r.end,r.name,r.kind,
                    ROW_NUMBER() OVER (
                        PARTITION BY o.file_id
                        ORDER BY CASE WHEN o.role='declaration' THEN 0
                                      WHEN o.role IN ('import','import_path') THEN 2
                                      ELSE 1 END,o.start,o.id
                    ) AS file_rank,
                    COUNT(*) OVER (PARTITION BY o.file_id) AS uses
             FROM occurrences o
             JOIN files f ON f.id=o.file_id
             JOIN regions r
               ON r.file_id=o.file_id AND r.start<=o.start AND o.end<=r.end
             WHERE o.name=?1{}
               AND (?2='' OR f.language=?2)
               AND (?3='' OR r.kind=?3)
               AND (?4 IS NULL OR o.file_id IN (SELECT file_id FROM occurrences WHERE name=?4))
         )
         SELECT f.path,f.revision,m.start,m.end,m.name,m.kind,NULL,
                'identifier_occurrence',c.bytes
         FROM matched m
         JOIN files f ON f.id=m.file_id
         JOIN contents c ON c.revision=f.revision
         WHERE m.file_rank=1
         ORDER BY m.uses DESC,f.path,m.start
         LIMIT ?5",
        paths.sql_clause("f.path")
    ))?;
    let mut hits = statement
        .query_map(
            params![
                identifier.name,
                query.language,
                query.kind,
                identifier.qualifier,
                (file_ranking::FILE_CANDIDATE_LIMIT + 1) as i64
            ],
            hit_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(super::cap_hits(
        &mut hits,
        file_ranking::FILE_CANDIDATE_LIMIT,
    ))
}
