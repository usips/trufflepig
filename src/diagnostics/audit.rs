use super::{
    DiagnosticStore, EventStage, RequestEvent, journal::RETENTION_SECONDS, records, sessions,
};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::Ordering;

#[derive(Debug, Serialize)]
pub struct AuditReport {
    pub schema_version: u32,
    pub observational_only: bool,
    pub retained_requests: usize,
    pub complete_deliveries: usize,
    pub incomplete_deliveries: usize,
    pub prepared_tokens: usize,
    pub stdout_accepted_bytes: usize,
    pub stderr_accepted_bytes: usize,
    pub surfaced_metadata_identities: usize,
    pub viewed_source_identities: usize,
    pub truncated_records: usize,
    pub dropped_records: u64,
    pub evicted_segments: u64,
    pub malformed_records: usize,
    pub incomplete_window: bool,
    pub loss_detection_complete: bool,
    pub residency_samples: usize,
    pub sessions: serde_json::Value,
    pub ledger: super::ledger::ObservationLedger,
}

#[derive(Default)]
struct RequestEvidence {
    server: bool,
    delivery: Option<RequestEvent>,
}

impl DiagnosticStore {
    pub fn render_audit(
        &self,
        budget: &crate::output::OutputBudget,
        session: Option<&str>,
    ) -> Result<String> {
        let mut value = serde_json::to_value(self.audit(session)?)?;
        if let Ok(response) = budget.render(&value) {
            return Ok(response);
        }
        value["output_truncated"] = true.into();
        value["detail_hint"] = "Increase --budget to inspect retained session details".into();
        if let Some(sessions) = value["sessions"]["sessions"].as_array_mut() {
            for session in sessions.iter_mut() {
                session
                    .as_object_mut()
                    .map(|session| session.remove("report"));
            }
        }
        let mut omitted = 0;
        loop {
            if let Ok(response) = budget.render(&value) {
                return Ok(response);
            }
            if value["sessions"]["sessions"]
                .as_array_mut()
                .and_then(|sessions| sessions.pop())
                .is_none()
            {
                return budget.render(&value);
            }
            omitted += 1;
            value["session_summaries_omitted"] = omitted.into();
        }
    }

    pub fn audit(&self, session: Option<&str>) -> Result<AuditReport> {
        let _guard = self
            .lock()?
            .ok_or_else(|| anyhow::anyhow!("diagnostics_busy"))?;
        let now = records::now();
        let forgotten: i64 = self.conn.query_row(
            "SELECT value FROM diagnostic_meta WHERE key='forgotten_micros'",
            [],
            |row| row.get(0),
        )?;
        let mut requests = HashMap::<String, RequestEvidence>::with_capacity(1024);
        let mut malformed = 0;
        let mut truncated = 0;
        let mut capped = false;
        let mut residency_samples = 0;
        let mut retained_bytes = 0_usize;
        for path in self.segments()? {
            let mut reader = BufReader::new(std::fs::File::open(path)?);
            let mut line = Vec::with_capacity(4096);
            while let Some(oversized) = read_record(&mut reader, &mut line)? {
                if oversized {
                    malformed += 1;
                    continue;
                }
                if line.is_empty() {
                    continue;
                }
                let Ok(event) = serde_json::from_slice::<RequestEvent>(&line) else {
                    malformed += 1;
                    continue;
                };
                if now.saturating_sub(event.timestamp) > RETENTION_SECONDS {
                    continue;
                }
                if event.context.created_unix_micros <= forgotten as u64 {
                    continue;
                }
                if session.is_some_and(|id| event.context.session.as_deref() != Some(id)) {
                    continue;
                }
                if event.truncated {
                    truncated += 1;
                }
                if event.stage == EventStage::Maintenance {
                    residency_samples += usize::from(event.residency.is_some());
                    continue;
                }
                if (requests.len() >= 10_000 || retained_bytes + line.len() > 32 * 1024 * 1024)
                    && !requests.contains_key(&event.context.request_id)
                {
                    capped = true;
                    continue;
                }
                retained_bytes += line.len();
                let evidence = requests
                    .entry(event.context.request_id.clone())
                    .or_default();
                match event.stage {
                    EventStage::Server => evidence.server = true,
                    EventStage::Delivery => evidence.delivery = Some(event),
                    EventStage::Maintenance => unreachable!(),
                }
            }
        }
        let mut report = AuditReport {
            schema_version: 1,
            observational_only: true,
            retained_requests: requests.len(),
            complete_deliveries: 0,
            incomplete_deliveries: 0,
            prepared_tokens: 0,
            stdout_accepted_bytes: 0,
            stderr_accepted_bytes: 0,
            surfaced_metadata_identities: 0,
            viewed_source_identities: 0,
            truncated_records: truncated,
            dropped_records: self.conn.query_row(
                "SELECT value FROM diagnostic_meta WHERE key='drops'",
                [],
                |r| r.get::<_, i64>(0),
            )? as u64
                + self.local_drops.load(Ordering::Relaxed),
            evicted_segments: self.conn.query_row(
                "SELECT value FROM diagnostic_meta WHERE key='evicted'",
                [],
                |r| r.get::<_, i64>(0),
            )? as u64,
            malformed_records: malformed,
            incomplete_window: capped || malformed > 0,
            loss_detection_complete: false,
            residency_samples,
            sessions: sessions::audit(&self.conn, session)?,
            ledger: super::ledger::ObservationLedger::default(),
        };
        let mut complete_events = Vec::with_capacity(requests.len());
        for evidence in requests.values() {
            report.stderr_accepted_bytes += evidence
                .delivery
                .as_ref()
                .map_or(0, |event| event.stderr_accepted_bytes);
            let receipt = evidence
                .delivery
                .as_ref()
                .and_then(|event| event.receipt.as_ref());
            if let Some(receipt) = receipt {
                report.prepared_tokens += receipt.prepared_tokens;
                report.stdout_accepted_bytes += receipt.accepted_bytes;
            }
            if !receipt.is_some_and(|receipt| {
                receipt.complete && receipt.accepted_bytes == receipt.prepared_bytes
            }) {
                report.incomplete_deliveries += 1;
                continue;
            }
            report.complete_deliveries += 1;
            let delivery = evidence
                .delivery
                .as_ref()
                .expect("complete receipt has event");
            complete_events.push(delivery);
            for identity in &delivery.emitted {
                if identity.source_body {
                    report.viewed_source_identities += 1;
                } else {
                    report.surfaced_metadata_identities += 1;
                }
            }
        }
        report.ledger = super::ledger::compare(&report.sessions, &complete_events);
        report.incomplete_window |= report.dropped_records > 0
            || report.evicted_segments > 0
            || truncated > 0
            || report.incomplete_deliveries > 0
            || report.ledger.incomplete_change_details > 0
            || report.ledger.incomplete_publication_windows > 0
            || !report.loss_detection_complete;
        Ok(report)
    }
}

fn read_record(reader: &mut impl BufRead, output: &mut Vec<u8>) -> std::io::Result<Option<bool>> {
    output.clear();
    let mut oversized = false;
    let mut read_any = false;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(read_any.then_some(oversized));
        }
        read_any = true;
        let newline = bytes.iter().position(|&byte| byte == b'\n');
        let count = newline.unwrap_or(bytes.len());
        if output.len() + count > 64 * 1024 {
            oversized = true;
        }
        if !oversized {
            output.extend_from_slice(&bytes[..count]);
        }
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(oversized));
        }
    }
}
