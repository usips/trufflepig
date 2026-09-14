use super::{ProbeName, ProbeOutcome, ProbeResult};
use crate::{
    extract, source,
    store::{Store, decode_path},
};
use anyhow::Result;
use rusqlite::OptionalExtension;
use std::{
    path::Path,
    time::{Duration, Instant},
};

const SAMPLE_BYTES: usize = 64 * 1024;
const SAMPLE_FILES: usize = 4;

pub(super) fn check(store: &Store, cache: &Path) -> ProbeResult {
    let started = Instant::now();
    let mut report = ProbeResult::new(ProbeName::UnchangedExtractionIdentity);
    let result = samples(store, cache, &mut report, started);
    if result.is_err() && !matches!(report.outcome, ProbeOutcome::Failed) {
        report.outcome = ProbeOutcome::Incomplete;
    }
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    report
}

fn samples(store: &Store, cache: &Path, report: &mut ProbeResult, started: Instant) -> Result<()> {
    let conn = super::connection(cache, "index.sqlite3", false)?;
    conn.execute_batch("BEGIN DEFERRED")?;
    let mut statement = conn.prepare(
        "SELECT path,revision FROM files WHERE revision IS NOT NULL AND language!='text' AND bytes<=?1 ORDER BY path LIMIT ?2"
    )?;
    let files = statement
        .query_map([SAMPLE_BYTES as i64, SAMPLE_FILES as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let cache_version = i64::from_le_bytes(
        blake3::hash(include_bytes!("../../Cargo.lock")).as_bytes()[..8]
            .try_into()
            .expect("eight hash bytes"),
    );
    for (path, revision) in files {
        if started.elapsed() >= Duration::from_millis(250) {
            if !matches!(report.outcome, ProbeOutcome::Failed) {
                report.outcome = ProbeOutcome::Incomplete;
            }
            break;
        }
        let bytes = match source::read_contained(&store.root, &decode_path(&path)?, SAMPLE_BYTES) {
            Ok(bytes) => bytes,
            Err(_) => {
                report.unverified += 1;
                continue;
            }
        };
        if blake3::hash(&bytes).to_hex().as_str() != revision {
            report.drifted += 1;
            continue;
        }
        let grammar = path.rsplit('.').next().unwrap_or("");
        let cached: Option<String> = conn.query_row(
            "SELECT facts FROM extraction_cache WHERE revision=?1 AND grammar=?2 AND version=?3 AND length(facts)<=1048576",
            rusqlite::params![revision, grammar, cache_version], |row| row.get(0)
        ).optional()?;
        let Some(cached) = cached else {
            report.unverified += 1;
            continue;
        };
        let extracted = extract::extract(&path, &bytes);
        if matches!(extracted.status.as_str(), "cancelled" | "fact_limit") {
            report.unverified += 1;
            continue;
        }
        let cached: serde_json::Value = match serde_json::from_str(&cached) {
            Ok(cached) => cached,
            Err(_) => {
                report.outcome = ProbeOutcome::Failed;
                continue;
            }
        };
        report.checked += 1;
        if cached["status"] == "fact_limit" || cached["status"] == "cancelled" {
            report.unverified += 1;
            continue;
        }
        if serde_json::to_value(&extracted)? != cached {
            report.outcome = ProbeOutcome::Failed;
        }
        match published_definitions_match(&conn, &path, &extracted)? {
            Some(false) => report.outcome = ProbeOutcome::Failed,
            None => report.unverified += 1,
            Some(true) => {}
        }
    }
    if report.unverified > 0 && !matches!(report.outcome, ProbeOutcome::Failed) {
        report.outcome = ProbeOutcome::Incomplete;
    } else if report.checked == 0 && matches!(report.outcome, ProbeOutcome::Passed) {
        report.outcome = if report.drifted > 0 {
            ProbeOutcome::Incomplete
        } else {
            ProbeOutcome::Unavailable
        };
    }
    Ok(())
}

fn published_definitions_match(
    conn: &rusqlite::Connection,
    path: &str,
    extracted: &extract::Extraction,
) -> Result<Option<bool>> {
    const MAX_DEFINITIONS: usize = 1024;
    if extracted.definitions.len() > MAX_DEFINITIONS {
        return Ok(None);
    }
    let mut query = conn.prepare(
        "SELECT d.name,d.kind,d.start,d.end,d.container FROM definitions d
         JOIN files f ON f.id=d.file_id WHERE f.path=?1
         AND NOT (d.kind='module' AND d.name=f.path AND d.start=0 AND d.container IS NULL)
         ORDER BY d.id LIMIT 1025",
    )?;
    let definitions = query
        .query_map([path], |row| {
            Ok(extract::Definition {
                name: row.get(0)?,
                kind: row.get(1)?,
                start: row.get::<_, i64>(2)? as usize,
                end: row.get::<_, i64>(3)? as usize,
                container: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if definitions.len() > MAX_DEFINITIONS {
        return Ok(None);
    }
    Ok(Some(
        serde_json::to_value(definitions)? == serde_json::to_value(&extracted.definitions)?,
    ))
}
