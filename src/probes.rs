//! Bounded, observational checks; drift and unsampled data are not corruption.

mod extraction_probe;
mod semantic_probe;
#[cfg(test)]
mod tests;

use crate::{
    semantic::SemanticSession,
    store::{Coverage, Store},
};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const SQL_BUDGET: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeOutcome {
    Passed,
    Failed,
    Incomplete,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeName {
    StructuralIntegrity,
    ForeignKeys,
    FtsIntegrity,
    UnchangedExtractionIdentity,
    SemanticProvenance,
    ModelInitialization,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProbeResult {
    pub name: ProbeName,
    pub outcome: ProbeOutcome,
    pub checked: usize,
    pub drifted: usize,
    pub unverified: usize,
    pub elapsed_ms: u64,
}

impl ProbeResult {
    fn new(name: ProbeName) -> Self {
        Self {
            name,
            outcome: ProbeOutcome::Passed,
            checked: 0,
            drifted: 0,
            unverified: 0,
            elapsed_ms: 0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub generation: i64,
    pub coverage: Coverage,
    pub semantic_feature: bool,
    pub semantic_loaded: bool,
    pub semantic_initializations: u64,
    pub tokenizer: &'static str,
    pub probes: Vec<ProbeResult>,
}

/// SQL checks stop at 100 ms; extraction samples at most four 64 KiB files.
pub fn doctor(store: &Store, cache: &Path, session: &SemanticSession) -> Result<DoctorReport> {
    let initializations = crate::semantic::model_initializations();
    let mut checks = Vec::with_capacity(6);
    for (name, sql) in [
        (ProbeName::StructuralIntegrity, "PRAGMA quick_check(1)"),
        (ProbeName::ForeignKeys, "PRAGMA foreign_key_check"),
    ] {
        checks.push(sql_probe(cache, name, sql));
    }
    checks.push(fts_probe(cache));
    checks.push(extraction_probe::check(store, cache));
    checks.push(semantic_probe::check(cache));
    let mut initialization = ProbeResult::new(ProbeName::ModelInitialization);
    initialization.checked = 1;
    if crate::semantic::model_initializations() != initializations {
        initialization.outcome = ProbeOutcome::Failed;
    }
    checks.push(initialization);
    Ok(DoctorReport {
        generation: store.generation()?,
        coverage: store.coverage()?,
        semantic_feature: cfg!(feature = "semantic"),
        semantic_loaded: session.is_loaded(),
        semantic_initializations: initializations,
        tokenizer: "o200k_base",
        probes: checks,
    })
}

fn connection(cache: &Path, filename: &str, writable: bool) -> Result<Connection> {
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let conn = Connection::open_with_flags(cache.join(filename), flags)?;
    conn.busy_timeout(Duration::from_millis(10))?;
    let deadline = Instant::now() + SQL_BUDGET;
    conn.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    Ok(conn)
}

fn sql_probe(cache: &Path, name: ProbeName, sql: &str) -> ProbeResult {
    let started = Instant::now();
    let mut report = ProbeResult::new(name);
    let result: Result<bool> = (|| {
        let conn = connection(cache, "index.sqlite3", false)?;
        let mut statement = conn.prepare(sql)?;
        let mut rows = statement.query([])?;
        Ok(match rows.next()? {
            Some(row) if name == ProbeName::StructuralIntegrity => row.get::<_, String>(0)? == "ok",
            Some(_) => false,
            None => true,
        })
    })();
    report.outcome = outcome(result);
    report.checked = usize::from(!matches!(report.outcome, ProbeOutcome::Incomplete));
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    report
}

fn fts_probe(cache: &Path) -> ProbeResult {
    let started = Instant::now();
    let mut report = ProbeResult::new(ProbeName::FtsIntegrity);
    let result: Result<bool> = (|| {
        let conn = connection(cache, "index.sqlite3", true)?;
        // FTS5 exposes its integrity verifier as a command; no repair is requested.
        conn.execute_batch("SAVEPOINT doctor_fts")?;
        let checked: rusqlite::Result<bool> = (|| {
            conn.execute(
                "INSERT INTO documents(documents) VALUES('integrity-check')",
                [],
            )?;
            conn.query_row(
                "SELECT NOT EXISTS(
                    SELECT 1 FROM regions r LEFT JOIN documents d ON d.rowid=r.id
                    WHERE d.rowid IS NULL OR d.name IS NOT r.name OR d.body IS NOT r.body
                    UNION ALL
                    SELECT 1 FROM documents d LEFT JOIN regions r ON r.id=d.rowid
                    WHERE r.id IS NULL LIMIT 1)",
                [],
                |row| row.get(0),
            )
        })();
        conn.progress_handler(0, None::<fn() -> bool>)?;
        conn.execute_batch("ROLLBACK TO doctor_fts; RELEASE doctor_fts")?;
        Ok(checked?)
    })();
    report.outcome = outcome(result);
    report.checked = usize::from(!matches!(report.outcome, ProbeOutcome::Incomplete));
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    report
}

fn outcome(result: Result<bool>) -> ProbeOutcome {
    match result {
        Ok(true) => ProbeOutcome::Passed,
        Ok(false) => ProbeOutcome::Failed,
        Err(error) => match error.downcast_ref::<rusqlite::Error>() {
            Some(rusqlite::Error::SqliteFailure(code, _))
                if matches!(
                    code.code,
                    rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                ) =>
            {
                ProbeOutcome::Failed
            }
            _ => ProbeOutcome::Incomplete,
        },
    }
}
