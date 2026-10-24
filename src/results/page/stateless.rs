use super::{SNIPPET_PAGE_FLOOR, largest_fitting};
use crate::{
    output::{OutputBudget, OutputFormat},
    results::{Hit, HitDetail, ResultEntry, ResultSet, compact_hit},
};
use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use std::path::Path;

/// Renders a current-source result without creating a persisted result set.
pub(crate) fn stateless_page(
    root: &Path,
    member: Option<&str>,
    set: &ResultSet,
    metadata: &Value,
    limit: usize,
    budget: &OutputBudget,
) -> Result<String> {
    let hits = set
        .hits
        .iter()
        .map(|hit| (hit, root, member))
        .collect::<Vec<_>>();
    render_ephemeral(
        &hits,
        set.coverage.clone(),
        metadata,
        set.truncated,
        limit,
        budget,
    )
}

/// Fairly merges member-local ranks and renders one cache-free page.
pub(crate) fn stateless_page_members(
    parts: &[(&Path, Option<&str>, &ResultSet)],
    metadata: &Value,
    limit: usize,
    budget: &OutputBudget,
) -> Result<String> {
    let mut ranked = Vec::with_capacity(parts.len());
    let mut source_truncated = false;
    for (root, member, set) in parts {
        source_truncated |= set.truncated;
        ranked.push((*root, *member, set.hits.as_slice()));
    }

    let total_hits = ranked
        .iter()
        .map(|(_, _, hits)| hits.len())
        .fold(0usize, usize::saturating_add);
    let mut hits = Vec::with_capacity(total_hits.min(limit).min(budget.limit));
    let mut rank = 0;
    while hits.len() < limit && hits.len() < budget.limit {
        let mut added = false;
        for (root, member, member_hits) in &ranked {
            if let Some(hit) = member_hits.get(rank) {
                hits.push((hit, *root, *member));
                added = true;
                if hits.len() >= limit || hits.len() >= budget.limit {
                    break;
                }
            }
        }
        if !added {
            break;
        }
        rank += 1;
    }

    let coverage = metadata
        .get("coverage")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let metadata_truncated = metadata["truncated"].as_bool().unwrap_or(false);
    let truncated = source_truncated
        || metadata_truncated
        || total_hits > hits.len()
        || parts
            .iter()
            .any(|(_, _, set)| set.coverage["absence"] == "not_proven");

    let mut envelope = metadata.clone();
    if !envelope.is_object() {
        envelope = json!({"metadata": envelope});
    }
    if let Some(object) = envelope.as_object_mut() {
        let members = object.entry("members").or_insert_with(|| json!({}));
        if let Some(members) = members.as_object_mut() {
            for (root, member, _) in &ranked {
                if let Some(member) = member {
                    members.insert((*member).to_owned(), crate::store::encode_path(root).into());
                }
            }
        }
        object.insert("coverage".into(), coverage);
        object.insert("truncated".into(), truncated.into());
    }
    render_ephemeral(&hits, Value::Null, &envelope, truncated, hits.len(), budget)
}

fn render_ephemeral(
    hits: &[(&Hit, &Path, Option<&str>)],
    coverage: Value,
    metadata: &Value,
    source_truncated: bool,
    limit: usize,
    budget: &OutputBudget,
) -> Result<String> {
    let max_count = hits.len().min(limit).min(budget.limit);
    let coverage = if coverage.is_null() {
        metadata
            .get("coverage")
            .cloned()
            .unwrap_or_else(|| json!({}))
    } else {
        coverage
    };
    let metadata_truncated = metadata["truncated"].as_bool().unwrap_or(false);
    let any_omitted = |count: usize| source_truncated || metadata_truncated || count < hits.len();
    let render = |count: usize, detail: HitDetail| -> Result<String> {
        match budget.format {
            OutputFormat::Json => {
                let mut value = metadata.as_object().cloned().unwrap_or_default();
                let mut rendered_hits = hits[..count]
                    .iter()
                    .map(|(hit, root, member)| compact_hit(hit, root, *member, detail))
                    .collect::<Result<Vec<_>>>()?;
                let truncated = any_omitted(count);
                value.insert("generation".into(), 0.into());
                value.insert("coverage".into(), coverage.clone());
                value.insert("tokenizer".into(), "o200k_base".into());
                value.insert(
                    "hits".into(),
                    Value::Array(std::mem::take(&mut rendered_hits)),
                );
                value.insert("next".into(), Value::Null);
                value.insert("truncated".into(), truncated.into());
                budget.encode(&Value::Object(value))
            }
            OutputFormat::Lines => {
                let entries = hits[..count]
                    .iter()
                    .map(|(hit, _, _)| ResultEntry::LiveSource((**hit).clone()))
                    .collect::<Vec<_>>();
                let rendered_entries = entries
                    .iter()
                    .zip(hits[..count].iter())
                    .map(|(entry, (_, _, member))| (*member, entry));
                let mut summary = if coverage.is_array() {
                    "workspace current-source coverage".to_owned()
                } else {
                    crate::results::lines::single_repo_coverage(&coverage)
                };
                if coverage["current_source"]["unsearched_unknown"] == true {
                    summary.push_str("; partial (current-source count unknown)");
                }
                let metadata_line = compact_metadata(metadata)?;
                let coverage_line = if metadata_line == "{}" {
                    summary
                } else {
                    format!("{summary}; metadata={metadata_line}")
                };
                Ok(crate::results::lines::page_text(
                    rendered_entries,
                    detail,
                    &coverage_line,
                    None,
                    any_omitted(count),
                ))
            }
        }
    };

    let snippet_floor = max_count.min(SNIPPET_PAGE_FLOOR);
    if snippet_floor > 0 {
        let full = HitDetail {
            names: true,
            snippets: true,
        };
        let count = largest_fitting(max_count, |count| Ok(budget.fits(&render(count, full)?)))?;
        if count >= snippet_floor {
            return render(count, full);
        }
    }
    for names in [false, true] {
        let detail = HitDetail {
            names,
            snippets: false,
        };
        let count = largest_fitting(max_count, |count| Ok(budget.fits(&render(count, detail)?)))?;
        if count > 0 || budget.fits(&render(0, detail)?) {
            return render(count, detail);
        }
    }
    bail!(coded!(
        BudgetTooSmall,
        "no stateless result page envelope fits; increase --budget"
    ))
}

fn compact_metadata(metadata: &Value) -> Result<String> {
    let value = match metadata {
        Value::Object(object) => {
            let mut compact = Map::new();
            for key in [
                "workspace",
                "source",
                "cache",
                "cache_fallback",
                "home",
                "scope",
                "coverage",
                "members",
                "truncated",
            ] {
                if let Some(value) = object.get(key) {
                    compact.insert(key.into(), value.clone());
                }
            }
            Value::Object(compact)
        }
        value => value.clone(),
    };
    serde_json::to_string(&value).map_err(Into::into)
}
