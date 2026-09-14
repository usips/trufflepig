use super::{ProbeName, ProbeOutcome, ProbeResult, connection};
use crate::semantic::{DIMENSIONS, INPUT_VERSION, MODEL_REVISION};
use anyhow::Result;
use std::{path::Path, time::Instant};

pub(super) fn check(cache: &Path) -> ProbeResult {
    let started = Instant::now();
    let mut report = ProbeResult::new(ProbeName::SemanticProvenance);
    if !cache.join("embeddings.sqlite").exists() {
        report.outcome = ProbeOutcome::Unavailable;
        return report;
    }
    if sample(cache, &mut report).is_err() && !matches!(report.outcome, ProbeOutcome::Failed) {
        report.outcome = ProbeOutcome::Incomplete;
    }
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    report
}

fn sample(cache: &Path, report: &mut ProbeResult) -> Result<()> {
    let conn = connection(cache, "embeddings.sqlite", false)?;
    let provenance: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='embedding_provenance')", [], |row| row.get(0)
    )?;
    if !provenance {
        report.unverified = 1;
        report.outcome = ProbeOutcome::Incomplete;
        return Ok(());
    }
    let mut statement = conn.prepare(
        "SELECT length(e.vector),substr(e.vector,1,3072),p.model_revision,p.input_version
         FROM embeddings e LEFT JOIN embedding_provenance p USING(key) ORDER BY e.key LIMIT 16",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let length: i64 = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        let revision: Option<String> = row.get(2)?;
        let input: Option<String> = row.get(3)?;
        report.checked += 1;
        let values = bytes
            .chunks_exact(4)
            .map(|part| f32::from_le_bytes(part.try_into().expect("four bytes")));
        let norm: f64 = values.clone().map(|value| f64::from(value).powi(2)).sum();
        if length != (DIMENSIONS * 4) as i64
            || values.clone().any(|value| !value.is_finite())
            || (norm - 1.0).abs() > 0.001
        {
            report.outcome = ProbeOutcome::Failed;
        }
        // Entries from another pinned model remain reusable cache data, not corruption.
        if revision.as_deref() != Some(MODEL_REVISION) || input.as_deref() != Some(INPUT_VERSION) {
            report.unverified += 1;
        }
    }
    if report.unverified > 0 && !matches!(report.outcome, ProbeOutcome::Failed) {
        report.outcome = ProbeOutcome::Incomplete;
    } else if report.checked == 0 {
        report.outcome = ProbeOutcome::Unavailable;
    }
    Ok(())
}
