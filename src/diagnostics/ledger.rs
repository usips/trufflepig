use super::RequestEvent;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Default, Serialize)]
pub struct ObservationLedger {
    pub available: bool,
    pub changed_preimage_files: usize,
    pub preimage_metadata_surfaced: usize,
    pub preimage_source_viewed: usize,
    pub changed_spans: usize,
    pub changed_spans_metadata_surfaced: usize,
    pub changed_spans_source_viewed: usize,
    pub additions_without_preimages: usize,
    pub coverage_changes: usize,
    pub uncertain_correspondence: usize,
    pub incomplete_publication_windows: usize,
    pub incomplete_change_details: usize,
    pub sessions_without_repository_identity: usize,
    pub unpublished_intermediate_edits_observable: bool,
}

pub(super) fn compare(sessions: &Value, deliveries: &[&RequestEvent]) -> ObservationLedger {
    let mut ledger = ObservationLedger::default();
    let Some(sessions) = sessions["sessions"].as_array() else {
        return ledger;
    };
    for session in sessions {
        let report = &session["report"];
        if report.is_null() {
            continue;
        }
        ledger.available = true;
        let repository = report["repository"]
            .as_str()
            .filter(|root| !root.is_empty());
        if repository.is_none() {
            ledger.sessions_without_repository_identity += 1;
            ledger.incomplete_change_details += 1;
        }
        ledger.incomplete_publication_windows +=
            usize::from(report["publication_observations"]["complete"] == false);
        ledger.incomplete_change_details +=
            report["changes_truncated"].as_u64().unwrap_or(0) as usize;
        ledger.incomplete_change_details += report["publication_observations_truncated"]
            .as_u64()
            .unwrap_or(0) as usize;
        let Some(changes) = report["changes"].as_array() else {
            continue;
        };
        for change in changes {
            ledger.incomplete_change_details +=
                change["source_changes"]["truncated"].as_u64().unwrap_or(0) as usize;
            ledger.incomplete_change_details +=
                change["occurrences"]["truncated"].as_u64().unwrap_or(0) as usize;
            if change["category"] == "addition_without_preimage" {
                ledger.additions_without_preimages += 1;
                continue;
            }
            if change["category"] == "coverage_change"
                || change["category"] == "ignored_or_excluded"
            {
                ledger.coverage_changes += 1;
            }
            let (Some(path), Some(revision)) =
                (change["path"].as_str(), change["before_revision"].as_str())
            else {
                continue;
            };
            ledger.changed_preimage_files += 1;
            let matches = deliveries
                .iter()
                .filter(|event| event.context.session.as_deref() == session["session"].as_str())
                .filter(|event| {
                    let start = report["before_capture_interval"][0]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_mul(1000);
                    let end = report["after_capture_interval"][1]
                        .as_u64()
                        .unwrap_or(u64::MAX / 1000)
                        .saturating_mul(1000);
                    event.context.created_unix_micros >= start
                        && event
                            .receipt
                            .as_ref()
                            .and_then(|receipt| receipt.completed_unix_micros)
                            .is_some_and(|completed| completed <= end)
                })
                .flat_map(|event| &event.emitted)
                .filter(|identity| {
                    repository
                        == Some(
                            identity
                                .owner_root
                                .as_deref()
                                .unwrap_or(&identity.repository),
                        )
                        && identity.path == path
                        && identity.content_revision == revision
                })
                .collect::<Vec<_>>();
            ledger.preimage_metadata_surfaced +=
                usize::from(matches.iter().any(|identity| !identity.source_body));
            ledger.preimage_source_viewed +=
                usize::from(matches.iter().any(|identity| {
                    identity.source_body && identity.start_byte < identity.end_byte
                }));
            if let Some(changes) = change["source_changes"]["changes"].as_array() {
                for source_change in changes {
                    compare_span(&source_change["before"], &matches, &mut ledger);
                }
            }
            if let Some(facts) = change["occurrences"]["changes"].as_array() {
                for fact in facts {
                    ledger.incomplete_change_details +=
                        usize::from(fact["candidates_truncated"] == true);
                    let correspondence = fact["correspondence"].as_str();
                    ledger.uncertain_correspondence += usize::from(
                        correspondence
                            .is_some_and(|s| s.contains("uncertain") || s.contains("candidate")),
                    );
                }
            }
        }
    }
    ledger
}

fn compare_span(
    preimage: &Value,
    evidence: &[&super::EmittedIdentity],
    ledger: &mut ObservationLedger,
) {
    let span = preimage.get("span").unwrap_or(preimage);
    let (Some(start), Some(end)) = (span["start"].as_u64(), span["end"].as_u64()) else {
        return;
    };
    if start >= end {
        return;
    }
    ledger.changed_spans += 1;
    let overlaps = |identity: &&&super::EmittedIdentity| {
        (identity.start_byte as u64) < end && (identity.end_byte as u64) > start
    };
    ledger.changed_spans_metadata_surfaced += usize::from(
        evidence
            .iter()
            .filter(overlaps)
            .any(|identity| !identity.source_body),
    );
    ledger.changed_spans_source_viewed += usize::from(
        evidence
            .iter()
            .filter(overlaps)
            .any(|identity| identity.source_body),
    );
}
