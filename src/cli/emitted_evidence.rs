//! Preserve only identities present in the final prepared JSON response.
use crate::diagnostics::{Outcome, RequestEvent};
use serde_json::Value;

pub(super) fn capture_emitted(root: &std::path::Path, response: &str, event: &mut RequestEvent) {
    let Ok(mut value) = serde_json::from_str::<Value>(response) else {
        return;
    };
    let fallback_repository = crate::store::encode_path(root);
    let repository = value["repository"]
        .as_str()
        .unwrap_or(&fallback_repository)
        .to_owned();
    qualify_workspace_evidence(&mut value, &repository);
    event.coverage = value
        .get("coverage")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    if value["hits"].as_array().is_some_and(Vec::is_empty) {
        event.outcome = Outcome::Empty;
    }
    let values = value
        .get("hits")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (rank, hit) in values.iter().enumerate().take(128) {
        if let Some(identity) = emitted_identity(&repository, hit, false, Some(rank + 1)) {
            event.emitted.push(identity);
        }
    }
    if let Some(hit) = value.get("hit") {
        if let Some(identity) = emitted_identity(&repository, hit, false, None) {
            event.emitted.push(identity);
        }
    }
    if let Some(edges) = value["relationships"].as_array() {
        if edges.len() > 50 {
            event.truncated = true;
        }
        for edge in edges.iter().take(50) {
            for side in ["source", "target"] {
                if let Some(identity) = emitted_identity(&repository, &edge[side], false, None) {
                    event.emitted.push(identity);
                }
            }
        }
    }
    if values.len() > 64 {
        event.truncated = true;
    }
    for hit in values.iter().take(64) {
        if let Some(identity) = emitted_identity(&repository, &hit["target"], false, None) {
            event.emitted.push(identity);
        }
        if let Some(candidates) = hit["candidates"].as_array() {
            if candidates.len() > 32 {
                event.truncated = true;
            }
            for candidate in candidates.iter().take(32) {
                if let Some(identity) = emitted_identity(&repository, candidate, false, None) {
                    event.emitted.push(identity);
                }
            }
        }
        for side in ["before", "after"] {
            if let Some(identity) = emitted_identity(
                &repository,
                &hit[side],
                false,
                hit["handle"]
                    .as_str()
                    .and_then(|h| h.rsplit_once(':'))
                    .and_then(|(_, n)| n.parse().ok()),
            ) {
                event.emitted.push(identity);
            }
        }
    }
    if let Some(hunks) = value["hunks"].as_array() {
        if hunks.len() > 64 {
            event.truncated = true;
        }
        for hunk in hunks.iter().take(64) {
            if let Some(hit) = values.iter().find(|hit| hit["handle"] == hunk["handle"]) {
                for side in ["before", "after"] {
                    if hunk[side]["text"].is_string() {
                        if let Some(mut identity) =
                            emitted_identity(&repository, &hit[side], true, None)
                        {
                            identity.start_byte = hunk[side]["start"]
                                .as_u64()
                                .unwrap_or(identity.start_byte as u64)
                                as usize;
                            identity.end_byte = hunk[side]["end"]
                                .as_u64()
                                .unwrap_or(identity.end_byte as u64)
                                as usize;
                            event.emitted.push(identity);
                        }
                    }
                }
            }
        }
    }
    if values.len() > 128 {
        event.truncated = true;
    }
    if event.emitted.len() > 128 {
        event.truncated = true;
        event.emitted.truncate(128);
    }
    if value.get("lines").is_some() {
        if let Some(mut identity) = emitted_identity(&repository, &value, true, None) {
            if let Some(lines) = value["lines"].as_array() {
                identity.start_byte = lines
                    .first()
                    .and_then(|v| v["start"].as_u64())
                    .unwrap_or(identity.start_byte as u64)
                    as usize;
                identity.end_byte = lines
                    .last()
                    .and_then(|v| v["end"].as_u64())
                    .unwrap_or(identity.start_byte as u64)
                    as usize;
            }
            event.emitted.push(identity);
        }
    }
}

fn qualify_workspace_evidence(value: &mut Value, fallback: &str) {
    let roots = value["members"].clone();
    let top_member = value["member"].as_str().map(str::to_owned);
    let qualify = |entry: &mut Value| {
        let member = entry["member"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| top_member.clone());
        let root = member
            .as_deref()
            .and_then(|name| roots[name].as_str())
            .unwrap_or(fallback);
        qualify_identity_tree(entry, root, member.as_deref());
    };
    if let Some(hits) = value["hits"].as_array_mut() {
        for hit in hits {
            qualify(hit);
        }
    }
    if let Some(hit) = value.get_mut("hit") {
        qualify(hit);
    }
    if let Some(edges) = value["relationships"].as_array_mut() {
        for edge in edges {
            qualify(edge);
        }
    }
}
fn qualify_identity_tree(value: &mut Value, root: &str, member: Option<&str>) {
    match value {
        Value::Object(object) => {
            if object.contains_key("path") {
                object.entry("owner_root").or_insert_with(|| root.into());
                object.entry("repository").or_insert_with(|| root.into());
                if let Some(member) = member {
                    object.entry("member").or_insert_with(|| member.into());
                }
            }
            for child in object.values_mut() {
                if child.is_object() || child.is_array() {
                    qualify_identity_tree(child, root, member);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                qualify_identity_tree(child, root, member);
            }
        }
        _ => {}
    }
}

fn emitted_identity(
    repository: &str,
    value: &Value,
    source_body: bool,
    original_rank: Option<usize>,
) -> Option<crate::diagnostics::EmittedIdentity> {
    Some(crate::diagnostics::EmittedIdentity {
        owner_root: Some(value["owner_root"].as_str().unwrap_or(repository).into()),
        member: value["member"].as_str().map(str::to_owned),
        repository: value["repository"]
            .as_str()
            .or_else(|| value["historical"]["repository"].as_str())
            .unwrap_or(repository)
            .into(),
        path: value["path"].as_str()?.into(),
        content_revision: value["revision"].as_str().unwrap_or("").into(),
        commit: value["commit"]
            .as_str()
            .or_else(|| value["historical"]["commit"].as_str())
            .map(str::to_owned),
        blob: value["blob"]
            .as_str()
            .or_else(|| value["historical"]["blob"].as_str())
            .map(str::to_owned),
        start_byte: value["start"]
            .as_u64()
            .or_else(|| value["span"]["start"].as_u64())
            .unwrap_or(0) as usize,
        end_byte: value["end"]
            .as_u64()
            .or_else(|| value["span"]["end"].as_u64())
            .unwrap_or(0) as usize,
        original_rank: value["handle"]
            .as_str()
            .and_then(|h| h.rsplit_once(':'))
            .and_then(|(_, n)| n.parse().ok())
            .or(original_rank),
        source_body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{Operation, RequestContext};
    use serde_json::json;

    #[test]
    fn diff_and_later_pages_preserve_preimage_identity() {
        let handle = "0123456789abcdef0123456789abcdef:27";
        let source = json!({"repository":"/repo/.git","path":"a.rs","revision":"original-digest","commit":"old-commit","blob":"old-blob","span":{"start":10,"end":40}});
        let response = json!({"hits":[{"entry":"change","handle":handle,"before":source}],"hunks":[{"handle":handle,"before":{"start":12,"end":18,"text":"secret source"}}]});
        let mut event = RequestEvent::new(
            RequestContext::new(None, None),
            Operation::Diff,
            Outcome::Success,
        );
        capture_emitted(
            std::path::Path::new("/repo"),
            &response.to_string(),
            &mut event,
        );
        assert_eq!(event.emitted.len(), 2);
        assert_eq!(event.emitted[0].original_rank, Some(27));
        assert_eq!(event.emitted[1].content_revision, "original-digest");
        assert_eq!(
            (event.emitted[1].start_byte, event.emitted[1].end_byte),
            (12, 18)
        );
        assert!(event.emitted[1].source_body);
        assert_eq!(event.emitted[1].blob.as_deref(), Some("old-blob"));
        assert!(
            !serde_json::to_string(&event)
                .unwrap()
                .contains("secret source")
        );
    }

    #[test]
    fn candidate_metadata_and_capture_loss_are_explicit() {
        let candidates: Vec<_> = (0..40)
            .map(|n| json!({"path":"a.rs","revision":"rev","start":n,"end":n+1}))
            .collect();
        let response = json!({"hits":[{"path":"b.rs","revision":"rev2","start":0,"end":10,"candidates":candidates}]});
        let mut event = RequestEvent::new(
            RequestContext::new(None, None),
            Operation::References,
            Outcome::Success,
        );
        capture_emitted(
            std::path::Path::new("/repo"),
            &response.to_string(),
            &mut event,
        );
        assert!(event.truncated);
        assert!(event.emitted.iter().any(|identity| identity.path == "a.rs"));
        assert!(event.emitted.iter().all(|identity| !identity.source_body));
    }
}
