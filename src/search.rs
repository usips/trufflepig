//! Deterministic exact, lexical and live-regex retrieval from one index snapshot.
mod file_ranking;
mod live;
mod navigation;
mod rerank_window;
mod semantic_lane;
mod snippets;
pub mod telemetry;
#[cfg(test)]
mod tests;
pub(crate) use file_ranking::fuse_search_file_lanes as fuse_file_lanes;
pub(crate) use navigation::context_entry;
pub use navigation::{context, map, references};
pub use rerank_window::RerankScorer;

use crate::{
    results::{Hit, MAX_HITS, ResultSet},
    source,
    store::Store,
};
use anyhow::{Result, bail};
use rusqlite::{Row, params};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
};

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
        snippet: None,
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
        false,
        cache,
        &mut session,
        &mut telemetry::RetrievalTrace::disabled(),
    )
}

pub fn search_with_session(
    store: &Store,
    query: &Query,
    semantic: bool,
    rerank: bool,
    cache: &std::path::Path,
    session: &mut crate::semantic::SemanticSession,
    trace: &mut telemetry::RetrievalTrace,
) -> Result<ResultSet> {
    use std::time::Instant;
    use telemetry::{Lane, LaneOutcome};
    trace.begin(&store.root);
    let preparation_started = Instant::now();
    let semantic_enabled = semantic && !query.exact && !query.regex;
    let (prepared, semantic_status) = match session.prepare(semantic_enabled, cache, &query.text) {
        Ok(prepared) => (prepared, None),
        Err(error) => {
            let status = format!("{error:#}");
            (None, Some(status))
        }
    };
    if semantic_enabled {
        trace.query_preparation_us = Some(telemetry::elapsed_us(preparation_started));
    }
    let preparation_us = trace.query_preparation_us;
    let reranker =
        (rerank && !query.exact && !query.regex).then_some(&*session as &dyn RerankScorer);
    let mut output = search_prepared(store, query, cache, prepared, reranker, trace);
    if let (Some(status), Ok(result)) = (&semantic_status, &mut output) {
        result.coverage["semantic_status"] = if status.starts_with("semantic_cache_locked:") {
            "partial"
        } else {
            "unavailable"
        }
        .into();
        result.coverage["semantic_reason"] = status.clone().into();
        trace.record(
            Lane::Semantic,
            preparation_started,
            &[],
            LaneOutcome::Failed,
            false,
        );
        if let Some(generation) = trace.generation {
            trace.snapshot(generation, &result.coverage);
        }
    }
    trace.query_preparation_us = preparation_us;
    output
}

/// Runs retrieval with a query embedding prepared once by the calling scope.
pub fn search_prepared(
    store: &Store,
    query: &Query,
    cache: &std::path::Path,
    semantic_query: Option<crate::semantic::Embedding>,
    reranker: Option<&dyn RerankScorer>,
    trace: &mut telemetry::RetrievalTrace,
) -> Result<ResultSet> {
    use std::time::Instant;
    use telemetry::{Lane, LaneOutcome};
    trace.begin(&store.root);
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
        let outcome = exact_hits(store, query);
        let exact = outcome
            .as_ref()
            .map(|lane| lane.hits.as_slice())
            .unwrap_or(&[]);
        trace.record(
            Lane::ExactIdentifier,
            started,
            exact,
            LaneOutcome::from_result(&outcome, false),
            outcome.as_ref().is_ok_and(|lane| lane.truncated),
        );
        let exact = outcome?;
        if !query.exact && !query.text.is_empty() {
            let terms = fts_terms(&query.text);
            if !terms.is_empty() {
                let started = Instant::now();
                let outcome = lexical_hits(store, query, &terms);
                let lexical = outcome
                    .as_ref()
                    .map(|lane| lane.hits.as_slice())
                    .unwrap_or(&[]);
                trace.record(
                    Lane::Lexical,
                    started,
                    lexical,
                    LaneOutcome::from_result(&outcome, false),
                    outcome.as_ref().is_ok_and(|lane| lane.truncated),
                );
                let lexical = outcome?;
                if !query.exact && (query.kind.is_empty() || query.kind == "file") {
                    let started = Instant::now();
                    let outcome = file_hits(store, query);
                    let files = outcome
                        .as_ref()
                        .map(|lane| lane.hits.as_slice())
                        .unwrap_or(&[]);
                    trace.record(
                        Lane::File,
                        started,
                        files,
                        LaneOutcome::from_result(&outcome, false),
                        outcome.as_ref().is_ok_and(|lane| lane.truncated),
                    );
                    let files = outcome?;
                    truncated |= exact.truncated || lexical.truncated || files.truncated;
                    hits = fuse_file_lanes([
                        exact.hits.as_slice(),
                        lexical.hits.as_slice(),
                        files.hits.as_slice(),
                    ]);
                } else {
                    truncated |= exact.truncated || lexical.truncated;
                    hits = fuse_file_lanes([exact.hits.as_slice(), lexical.hits.as_slice()]);
                }
            } else {
                truncated |= exact.truncated;
                hits = exact.hits;
            }
        } else if !query.exact && (query.kind.is_empty() || query.kind == "file") {
            let started = Instant::now();
            let outcome = file_hits(store, query);
            let files = outcome
                .as_ref()
                .map(|lane| lane.hits.as_slice())
                .unwrap_or(&[]);
            trace.record(
                Lane::File,
                started,
                files,
                LaneOutcome::from_result(&outcome, false),
                outcome.as_ref().is_ok_and(|lane| lane.truncated),
            );
            let files = outcome?;
            truncated |= exact.truncated || files.truncated;
            hits = fuse_file_lanes([exact.hits.as_slice(), files.hits.as_slice()]);
        } else {
            truncated |= exact.truncated;
            hits = exact.hits;
            if query.exact {
                // `sym:` pages lead with declarations, not imports that share the name.
                hits.sort_by_key(|hit| declaration_rank(&hit.kind));
            }
        }
    }
    if !query.exact
        && !query.regex
        && let Some(vector) = semantic_query.as_ref()
    {
        truncated |=
            semantic_lane::append(store, query, cache, vector, &mut hits, &mut coverage, trace)?;
    }
    if let Some(scorer) = reranker {
        rerank_window::apply(
            store,
            &query.text,
            cache,
            scorer,
            &mut hits,
            &mut coverage,
            trace,
        );
    }
    trace.snapshot(generation, &coverage);
    let mut seen = HashSet::with_capacity(hits.len());
    hits.retain(|hit| seen.insert((hit.path.clone(), hit.start, hit.end, hit.kind.clone())));
    if hits.len() > MAX_HITS {
        hits.truncate(MAX_HITS);
        truncated = true;
    }
    snippets::attach(store, &snippets::preview_terms(&query.text), &mut hits)?;
    snapshot.commit()?;
    Ok(ResultSet {
        generation,
        coverage,
        truncated,
        hits,
    })
}

/// Every indexed definition named exactly `query.text` (honoring `file:`, `lang:`,
/// and `kind:`), declarations before members and locals, then in path order.
pub fn definitions(store: &Store, query: &Query) -> Result<ResultSet> {
    let snapshot = store.conn.unchecked_transaction()?;
    let generation = store.generation()?;
    let coverage = serde_json::to_value(store.coverage()?)?;
    let exact = Query {
        text: query.text.clone(),
        path: query.path.clone(),
        language: query.language.clone(),
        kind: query.kind.clone(),
        exact: true,
        regex: false,
    };
    let LaneHits {
        mut hits,
        truncated,
    } = exact_hits(store, &exact)?;
    hits.sort_by_key(|hit| declaration_rank(&hit.kind));
    snippets::attach(store, &snippets::preview_terms(&exact.text), &mut hits)?;
    snapshot.commit()?;
    Ok(ResultSet {
        generation,
        coverage,
        truncated,
        hits,
    })
}

/// Declarations a reader usually means by a name rank before modules, modules
/// before members, and members before locals and imports that merely share it.
fn declaration_rank(kind: &str) -> u8 {
    match kind {
        "module" => 1,
        "variant" | "field" => 2,
        "variable" | "parameter" | "import" => 3,
        _ => 0,
    }
}

#[derive(Debug)]
struct LaneHits {
    hits: Vec<Hit>,
    truncated: bool,
}

fn cap_lane(mut hits: Vec<Hit>) -> LaneHits {
    cap_hits(&mut hits, file_ranking::FILE_CANDIDATE_LIMIT)
}

fn cap_hits(hits: &mut Vec<Hit>, limit: usize) -> LaneHits {
    let truncated = hits.len() > limit;
    hits.truncate(limit);
    LaneHits {
        hits: std::mem::take(hits),
        truncated,
    }
}

fn exact_hits(store: &Store, query: &Query) -> Result<LaneHits> {
    if query.exact {
        let mut statement = store.conn.prepare(
            "SELECT f.path,f.revision,d.start,d.end,d.name,d.kind,d.container,
                    'exact_identifier',c.bytes
             FROM definitions d
             JOIN files f ON f.id=d.file_id
             JOIN contents c ON c.revision=f.revision
             WHERE d.name=?1
               AND substr(f.path,1,length(?2))=?2
               AND (?3='' OR f.language=?3)
               AND (?4='' OR d.kind=?4)
             ORDER BY f.path,d.start,d.end,d.id
             LIMIT ?5",
        )?;
        let mut hits = statement
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
            .collect::<rusqlite::Result<Vec<_>>>()?;
        return Ok(cap_hits(&mut hits, MAX_HITS));
    }
    let mut statement = store.conn.prepare(
        "WITH ranked AS (
             SELECT f.path,f.revision,d.start,d.end,d.name,d.kind,d.container,
                    'exact_identifier' AS provenance,c.bytes,d.id,
                    ROW_NUMBER() OVER (
                        PARTITION BY f.path ORDER BY d.start,d.end,d.id
                    ) AS file_rank
             FROM definitions d
             JOIN files f ON f.id=d.file_id
             JOIN contents c ON c.revision=f.revision
             WHERE (?1='' OR d.name=?1)
               AND substr(f.path,1,length(?2))=?2
               AND (?3='' OR f.language=?3)
               AND (?4='' OR d.kind=?4)
         )
         SELECT path,revision,start,end,name,kind,container,provenance,bytes
         FROM ranked
         WHERE file_rank=1
         ORDER BY path,start,end,id
         LIMIT ?5",
    )?;
    let hits = statement
        .query_map(
            params![
                query.text,
                query.path,
                query.language,
                query.kind,
                (file_ranking::FILE_CANDIDATE_LIMIT + 1) as i64
            ],
            hit_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cap_lane(hits))
}

fn lexical_hits(store: &Store, query: &Query, terms: &str) -> Result<LaneHits> {
    let mut statement = store.conn.prepare(
        "WITH ranked AS MATERIALIZED (
             SELECT f.path,f.revision,r.start,r.end,r.name,r.kind,NULL AS container,
                    'lexical' AS provenance,c.bytes,
                    bm25(documents,8.0,2.0,1.0,0.5) AS relevance,r.id
             FROM documents
             JOIN regions r ON r.id=documents.rowid
             JOIN files f ON f.id=r.file_id
             JOIN contents c ON c.revision=f.revision
             WHERE documents MATCH ?1
               AND substr(f.path,1,length(?2))=?2
               AND (?3='' OR f.language=?3)
               AND (?4='' OR r.kind=?4)
         )
         ,numbered AS (
             SELECT ranked.*,
                    ROW_NUMBER() OVER (
                        PARTITION BY path ORDER BY relevance,start,end,id
                    ) AS file_rank
             FROM ranked
         )
         SELECT path,revision,start,end,name,kind,container,provenance,bytes
         FROM numbered
         WHERE file_rank=1
         ORDER BY relevance,path,start,end,id
         LIMIT ?5",
    )?;
    let hits = statement
        .query_map(
            params![
                terms,
                query.path,
                query.language,
                query.kind,
                (file_ranking::FILE_CANDIDATE_LIMIT + 1) as i64
            ],
            hit_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cap_lane(hits))
}

fn fts_terms(text: &str) -> String {
    text.split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|s| !s.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn file_hits(store: &Store, query: &Query) -> Result<LaneHits> {
    if !query.kind.is_empty() && query.kind != "file" {
        return Ok(LaneHits {
            hits: Vec::new(),
            truncated: false,
        });
    }
    let mut stmt = store.conn.prepare(
        "SELECT f.path,f.revision
         FROM files f
         WHERE substr(f.path,1,length(?1))=?1
           AND (?2='' OR f.language=?2)
         ORDER BY f.path",
    )?;
    let rows = stmt.query_map(params![query.path, query.language], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let terms = query_terms(&query.text);
    let mut candidates = BinaryHeap::with_capacity(file_ranking::FILE_CANDIDATE_LIMIT + 1);
    let mut truncated = false;
    for row in rows {
        let (path, revision) = row?;
        let score = filename_score(&path, &terms);
        if score == 0 {
            continue;
        }
        let candidate = FilenameCandidate {
            score,
            hit: Hit {
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
                provenance: Some(
                    match score {
                        3 => "filename_exact",
                        2 => "filename_tokens",
                        _ => "filename_partial",
                    }
                    .into(),
                ),
                resolution: None,
                candidates: Vec::new(),
                target: None,
                snippet: None,
            },
        };
        candidates.push(candidate);
        if candidates.len() > file_ranking::FILE_CANDIDATE_LIMIT {
            candidates.pop();
            truncated = true;
        }
    }
    let mut candidates = candidates.into_vec();
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.hit.path.cmp(&right.hit.path))
    });
    Ok(LaneHits {
        hits: candidates
            .into_iter()
            .map(|candidate| candidate.hit)
            .collect(),
        truncated,
    })
}

struct FilenameCandidate {
    score: u8,
    hit: Hit,
}

impl Ord for FilenameCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // The heap keeps the least useful candidate at its root for eviction.
        other
            .score
            .cmp(&self.score)
            .then_with(|| self.hit.path.cmp(&other.hit.path))
            .then_with(|| self.hit.start.cmp(&other.hit.start))
            .then_with(|| self.hit.end.cmp(&other.hit.end))
    }
}

impl PartialOrd for FilenameCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for FilenameCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for FilenameCandidate {}

fn query_terms(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn filename_score(path: &str, terms: &[String]) -> u8 {
    if terms.is_empty() {
        return 1;
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    let lower_basename = basename.to_lowercase();
    let lower_path = path.to_lowercase();
    let stem = basename.rsplit_once('.').map_or(basename, |(stem, _)| stem);
    if query_terms(stem) == terms {
        return 3;
    }
    if terms.iter().all(|term| lower_basename.contains(term)) {
        return 2;
    }
    if terms.iter().any(|term| lower_path.contains(term)) {
        return 1;
    }
    0
}
