//! Deterministic exact, lexical and live-regex retrieval from one index snapshot.
mod live;
mod navigation;
mod semantic_lane;
pub mod telemetry;
#[cfg(test)]
mod tests;
pub use navigation::{context, map, references};

use crate::{
    results::{Hit, MAX_HITS, ResultSet},
    source,
    store::Store,
};
use anyhow::{Result, bail};
use rusqlite::{Row, params};
use std::collections::HashSet;

#[derive(Default, Debug)]
pub struct Query {
    pub text: String,
    pub path: String,
    pub language: String,
    pub kind: String,
    pub exact: bool,
    pub regex: bool,
}

impl Query {
    pub fn parse(input: &str) -> Result<Self> {
        let mut query = Self::default();
        let mut text = String::with_capacity(input.len());
        for chunk in input.split_inclusive(char::is_whitespace) {
            let word = chunk.trim_end();
            if let Some(value) = word.strip_prefix("file:") {
                query.path = value.to_owned();
            } else if let Some(value) = word.strip_prefix("lang:") {
                query.language = match value {
                    "rs" => "rust",
                    "ts" => "typescript",
                    "js" => "javascript",
                    "lua" => "luau",
                    "dm" => "dreammaker",
                    other => other,
                }
                .to_owned();
            } else if let Some(value) = word.strip_prefix("kind:") {
                query.kind = value.to_owned();
            } else {
                text.push_str(chunk);
            }
        }
        query.text = text.trim().to_owned();
        if let Some(text) = query.text.strip_prefix("sym:") {
            query.exact = true;
            query.text = text.to_owned();
        }
        if let Some(text) = query.text.strip_prefix("re:") {
            query.regex = true;
            query.text = text.to_owned();
        }
        if query.text.is_empty() && (query.exact || query.regex) {
            bail!("invalid_query: empty prefixed query");
        }
        Ok(query)
    }
}

pub(super) fn hit_row(row: &Row<'_>) -> rusqlite::Result<Hit> {
    let bytes: Vec<u8> = row.get(8)?;
    let start = row.get::<_, i64>(2)? as usize;
    let end = row.get::<_, i64>(3)? as usize;
    let (start_line, end_line) = source::line_span(&bytes, start, end);
    Ok(Hit {
        handle: String::new(),
        path: row.get(0)?,
        revision: row.get(1)?,
        start,
        end,
        start_line,
        end_line,
        name: row.get(4)?,
        kind: row.get(5)?,
        container: row.get(6)?,
        provenance: Some(row.get(7)?),
        resolution: None,
        candidates: Vec::new(),
        target: None,
    })
}

pub fn search(
    store: &Store,
    query: &Query,
    semantic: bool,
    cache: &std::path::Path,
) -> Result<ResultSet> {
    // Query inference precedes the snapshot. Candidate vectors use source bodies from that snapshot.
    let mut session = crate::semantic::SemanticSession::default();
    search_with_session(
        store,
        query,
        semantic,
        cache,
        &mut session,
        &mut telemetry::RetrievalTrace::disabled(),
    )
}

pub fn search_with_session(
    store: &Store,
    query: &Query,
    semantic: bool,
    cache: &std::path::Path,
    session: &mut crate::semantic::SemanticSession,
    trace: &mut telemetry::RetrievalTrace,
) -> Result<ResultSet> {
    use std::time::Instant;
    use telemetry::{Lane, LaneOutcome};
    trace.begin(&store.root);
    let preparation_started = Instant::now();
    let prepared = session.prepare(semantic, cache, &query.text);
    if semantic {
        trace.query_preparation_us = Some(telemetry::elapsed_us(preparation_started));
    }
    if prepared.is_err() {
        trace.record(
            Lane::Semantic,
            preparation_started,
            &[],
            LaneOutcome::Failed,
            false,
        );
    }
    let mut semantic_query = prepared?;
    let snapshot = store.conn.unchecked_transaction()?;
    let generation = store.generation()?;
    let mut coverage = serde_json::to_value(store.coverage()?)?;
    trace.snapshot(generation, &coverage);
    let mut hits = Vec::with_capacity(128);
    let mut truncated = false;
    if query.regex {
        let started = Instant::now();
        let outcome = live::live_regex(
            store,
            query,
            cache,
            &mut hits,
            &mut coverage,
            &mut truncated,
        );
        trace.record(
            Lane::LiveRegex,
            started,
            &hits,
            LaneOutcome::from_result(
                &outcome,
                coverage["live_read_failures"].as_u64().unwrap_or(0) > 0
                    || coverage["live_walk_failures"].as_u64().unwrap_or(0) > 0,
            ),
            truncated,
        );
        trace.snapshot(generation, &coverage);
        outcome?;
    } else {
        let started = Instant::now();
        let outcome = exact_hits(store, query, &mut hits);
        trace.record(
            Lane::ExactIdentifier,
            started,
            &hits,
            LaneOutcome::from_result(&outcome, false),
            hits.len() > MAX_HITS,
        );
        outcome?;
        if !query.exact && !query.text.is_empty() {
            let terms = fts_terms(&query.text);
            if !terms.is_empty() {
                let started = Instant::now();
                let first = hits.len();
                let outcome = lexical_hits(store, query, &terms, &mut hits);
                trace.record(
                    Lane::Lexical,
                    started,
                    &hits[first..],
                    LaneOutcome::from_result(&outcome, false),
                    hits.len() - first > MAX_HITS,
                );
                outcome?;
            }
        }
        if !query.exact && (query.kind.is_empty() || query.kind == "file") {
            let started = Instant::now();
            let first = hits.len();
            let outcome = file_hits(store, query, &mut hits);
            trace.record(
                Lane::File,
                started,
                &hits[first..],
                LaneOutcome::from_result(&outcome, false),
                hits.len() - first > MAX_HITS,
            );
            outcome?;
        }
    }
    if let Some((engine, vector)) = &mut semantic_query {
        truncated |= semantic_lane::append(
            store,
            query,
            engine,
            vector,
            &mut hits,
            &mut coverage,
            trace,
        )?;
    }
    trace.snapshot(generation, &coverage);
    let mut seen = HashSet::with_capacity(hits.len());
    hits.retain(|hit| seen.insert((hit.path.clone(), hit.start, hit.end, hit.kind.clone())));
    if hits.len() > MAX_HITS {
        hits.truncate(MAX_HITS);
        truncated = true;
    }
    snapshot.commit()?;
    Ok(ResultSet {
        generation,
        coverage,
        truncated,
        hits,
    })
}

fn exact_hits(store: &Store, query: &Query, hits: &mut Vec<Hit>) -> Result<()> {
    let mut statement=store.conn.prepare("SELECT f.path,f.revision,d.start,d.end,d.name,d.kind,d.container,'exact_identifier',c.bytes FROM definitions d JOIN files f ON f.id=d.file_id JOIN contents c ON c.revision=f.revision WHERE (?1='' OR d.name=?1) AND substr(f.path,1,length(?2))=?2 AND (?3='' OR f.language=?3) AND (?4='' OR d.kind=?4) ORDER BY f.path,d.start,d.end,d.id LIMIT ?5")?;
    hits.extend(
        statement
            .query_map(
                params![
                    query.text,
                    query.path,
                    query.language,
                    query.kind,
                    (MAX_HITS + 1) as i64
                ],
                hit_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    Ok(())
}

fn lexical_hits(store: &Store, query: &Query, terms: &str, hits: &mut Vec<Hit>) -> Result<()> {
    let mut statement=store.conn.prepare("SELECT f.path,f.revision,r.start,r.end,r.name,r.kind,NULL,'lexical',c.bytes FROM documents JOIN regions r ON r.id=documents.rowid JOIN files f ON f.id=r.file_id JOIN contents c ON c.revision=f.revision WHERE documents MATCH ?1 AND substr(f.path,1,length(?2))=?2 AND (?3='' OR f.language=?3) AND (?4='' OR r.kind=?4) ORDER BY bm25(documents,8.0,2.0,1.0,0.5),f.path,r.start,r.end,r.id LIMIT ?5")?;
    hits.extend(
        statement
            .query_map(
                params![
                    terms,
                    query.path,
                    query.language,
                    query.kind,
                    (MAX_HITS + 1) as i64
                ],
                hit_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    Ok(())
}

fn fts_terms(text: &str) -> String {
    text.split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|s| !s.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn file_hits(store: &Store, query: &Query, hits: &mut Vec<Hit>) -> Result<()> {
    if !query.kind.is_empty() && query.kind != "file" {
        return Ok(());
    }
    let mut stmt=store.conn.prepare("SELECT f.path,f.revision,f.language,f.status FROM files f WHERE substr(f.path,1,length(?1))=?1 AND (?2='' OR f.language=?2) AND (?3='' OR instr(lower(f.path),lower(?3))>0) ORDER BY f.path LIMIT ?4")?;
    let rows = stmt.query_map(
        params![
            query.path,
            query.language,
            query.text,
            (MAX_HITS + 1) as i64
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        },
    )?;
    for row in rows {
        let (path, revision, _language, status) = row?;
        hits.push(Hit {
            handle: String::new(),
            path: path.clone(),
            revision,
            start: 0,
            end: 0,
            start_line: 1,
            end_line: 1,
            name: path,
            kind: "file".into(),
            container: None,
            provenance: Some(status),
            resolution: None,
            candidates: Vec::new(),
            target: None,
        });
    }
    Ok(())
}
