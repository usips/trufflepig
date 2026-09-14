//! Owner summaries retain repository provenance and outcomes within the final budget.
use crate::output::OutputBudget;
use anyhow::{Result, bail};
use serde_json::{Value, json};

pub(super) fn render(mut value: Value, budget: &OutputBudget) -> Result<String> {
    if let Ok(output) = budget.render(&value) {
        return Ok(output);
    }
    value["output_truncated"] = true.into();
    if value["session"].is_string() && value["status"].is_string() {
        compact_session(&mut value);
        if let Ok(output) = budget.render(&value) {
            return Ok(output);
        }
    }
    if let Some(sessions) = value["sessions"]["sessions"].as_array_mut() {
        let mut reports_omitted = 0;
        for session in sessions {
            if session
                .as_object_mut()
                .and_then(|object| object.remove("report"))
                .is_some_and(|report| !report.is_null())
            {
                reports_omitted += 1;
            }
        }
        value["session_reports_omitted"] = reports_omitted.into();
        value["detail_hint"] = "Increase --budget to inspect retained session details".into();
        let mut omitted = 0;
        loop {
            if let Ok(output) = budget.render(&value) {
                return Ok(output);
            }
            if value["sessions"]["sessions"]
                .as_array_mut()
                .and_then(Vec::pop)
                .is_none()
            {
                break;
            }
            omitted += 1;
            value["session_summaries_omitted"] = omitted.into();
        }
    }
    if let Some(probes) = value["probes"].as_array_mut() {
        let mut omitted = 0;
        for probe in probes {
            if let Some(object) = probe.as_object_mut() {
                for field in ["checked", "drifted", "unverified", "elapsed_ms"] {
                    omitted += usize::from(object.remove(field).is_some());
                }
            }
        }
        value["probe_detail_fields_omitted"] = omitted.into();
        if let Ok(output) = budget.render(&value) {
            return Ok(output);
        }
    }
    bail!("budget_too_small: owner response and provenance do not fit")
}

fn compact_session(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let changed_files = object
        .get("changes")
        .and_then(Value::as_array)
        .map(Vec::len);
    let observations_complete = object
        .get("publication_observations")
        .and_then(|observations| observations.get("complete"))
        .cloned();
    let original_fields = object.len();
    object.retain(|key, _| {
        matches!(
            key.as_str(),
            "member"
                | "repository"
                | "workspace"
                | "session"
                | "status"
                | "baseline_generation"
                | "before_generation"
                | "after_generation"
                | "changes_truncated"
                | "overlapping_observation"
                | "ownership"
                | "publication_index_replaced"
                | "publication_observations_unavailable"
                | "output_truncated"
        )
    });
    let omitted = original_fields - object.len();
    object.insert("detail_fields_omitted".into(), omitted.into());
    if let Some(count) = changed_files {
        object.insert("retained_changed_files".into(), count.into());
    }
    if let Some(complete) = observations_complete {
        object.insert("publication_observations_complete".into(), complete);
    }
    object.insert(
        "details".into(),
        json!(format!(
            "audit {}",
            object["session"].as_str().unwrap_or_default()
        )),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(value: &mut Value) {
        value["member"] = "engine".into();
        value["workspace"] = "space".into();
        value["repository"] = "/work/engine".into();
    }

    #[test]
    fn session_summary_retains_handle_status_and_owner() {
        let mut value = json!({"session":"abc123","status":"ended","changes":[{"details":"long detail ".repeat(2000)}],"changes_truncated":4,"overlapping_observation":true});
        provenance(&mut value);
        let output = render(value, &OutputBudget::new(250).unwrap()).unwrap();
        let result: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["session"], "abc123");
        assert_eq!(result["status"], "ended");
        assert_eq!(result["repository"], "/work/engine");
        assert_eq!(result["member"], "engine");
        assert_eq!(result["workspace"], "space");
        assert_eq!(result["changes_truncated"], 4);
        assert_eq!(result["retained_changed_files"], 1);
        assert_eq!(result["output_truncated"], true);
        assert_eq!(result["details"], "audit abc123");
    }

    #[test]
    fn tiny_session_budget_never_discards_identity() {
        let mut value = json!({"session":"abc123","status":"open"});
        provenance(&mut value);
        assert!(
            render(value, &OutputBudget::new(5).unwrap())
                .unwrap_err()
                .to_string()
                .starts_with("budget_too_small:")
        );
    }

    #[test]
    fn audit_compacts_reports_and_keeps_incomplete_outcome() {
        let mut value = json!({"sessions":{"sessions":[{"session":"abc","status":"ended","report":{"changes":"detail ".repeat(2000)}}]},"incomplete_window":true});
        provenance(&mut value);
        let output = render(value, &OutputBudget::new(200).unwrap()).unwrap();
        let result: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["incomplete_window"], true);
        assert_eq!(result["session_reports_omitted"], 1);
        assert_eq!(result["sessions"]["sessions"][0]["status"], "ended");
    }

    #[test]
    fn probe_compaction_keeps_each_failure_outcome() {
        let mut value = json!({"probes":[{"name":"structural_integrity","outcome":"failed","checked":"detail ".repeat(2000),"drifted":0,"unverified":0,"elapsed_ms":1}]});
        provenance(&mut value);
        let output = render(value, &OutputBudget::new(150).unwrap()).unwrap();
        let result: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["probes"][0]["name"], "structural_integrity");
        assert_eq!(result["probes"][0]["outcome"], "failed");
        assert_eq!(result["probe_detail_fields_omitted"], 4);
    }
}
