//! Free-text query shapes and the lexical lanes they run. A literal `"…"`
//! query runs only the phrase lane. Other text runs, in fusion order: exact
//! definitions, identifier occurrences (one identifier-shaped token), phrase
//! and all-terms lanes (two or more terms), the any-term lane, and filenames.
use super::{
    LaneHits, Query, file_hits, file_ranking, hit_row, lexical_hits,
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
        let literal = text.len() >= 2 && text.starts_with('"') && text.ends_with('"');
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
            any_terms: Some(quoted.join(" OR ")),
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
        add(recorded(trace, Lane::IdentifierOccurrence, || {
            occurrence_hits(store, query, identifier)
        }))?;
    }
    if let Some(phrase) = &concept.phrase {
        add(recorded(trace, Lane::Phrase, || {
            lexical_hits(store, query, phrase, "phrase")
        }))?;
    }
    if let Some(all_terms) = &concept.all_terms {
        add(recorded(trace, Lane::AllTerms, || {
            lexical_hits(store, query, all_terms, "all_terms")
        }))?;
    }
    if let Some(any_terms) = &concept.any_terms {
        add(recorded(trace, Lane::Lexical, || {
            lexical_hits(store, query, any_terms, "lexical")
        }))?;
        if query.kind.is_empty() || query.kind == "file" {
            add(recorded(trace, Lane::File, || file_hits(store, query)))?;
        }
    }
    Ok((lanes, truncated))
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
fn occurrence_hits(store: &Store, query: &Query, identifier: &IdentifierToken) -> Result<LaneHits> {
    let mut statement = store.conn.prepare(
        "WITH matched AS (
             SELECT o.file_id,o.start,o.end,
                    ROW_NUMBER() OVER (
                        PARTITION BY o.file_id
                        ORDER BY CASE WHEN o.role='declaration' THEN 0
                                      WHEN o.role IN ('import','import_path') THEN 2
                                      ELSE 1 END,o.start,o.id
                    ) AS file_rank,
                    COUNT(*) OVER (PARTITION BY o.file_id) AS uses
             FROM occurrences o
             JOIN files f ON f.id=o.file_id
             WHERE o.name=?1
               AND substr(f.path,1,length(?2))=?2
               AND (?3='' OR f.language=?3)
               AND (?5 IS NULL OR EXISTS(
                   SELECT 1 FROM occurrences q WHERE q.file_id=o.file_id AND q.name=?5))
         )
         SELECT f.path,f.revision,r.start,r.end,r.name,r.kind,NULL,
                'identifier_occurrence',c.bytes
         FROM matched m
         JOIN files f ON f.id=m.file_id
         JOIN contents c ON c.revision=f.revision
         JOIN regions r ON r.file_id=m.file_id AND r.start<=m.start AND m.end<=r.end
         WHERE m.file_rank=1 AND (?4='' OR r.kind=?4)
         ORDER BY m.uses DESC,f.path,r.start
         LIMIT ?6",
    )?;
    let mut hits = statement
        .query_map(
            params![
                identifier.name,
                query.path,
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
