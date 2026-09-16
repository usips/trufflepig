//! Budgeted result pages over one repository's immutable result set.
//! Fitting binary-searches the hit count against the rendered text, JSON or
//! lines; names are dropped first and a lone ambiguous JSON hit trims candidates.

use super::{StoredResultSet, compact_entry_for_page, lines, load_entries};
use crate::{
    identity::ResultCursor,
    output::{OutputBudget, OutputFormat},
    store::Store,
};
use anyhow::{Result, bail};
use serde_json::Value;
use std::path::Path;

pub fn page(
    store: &Store,
    id: &str,
    offset: usize,
    limit: usize,
    budget: &OutputBudget,
) -> Result<String> {
    let set = load_entries(store, id)?;
    if offset > set.hits.len() {
        bail!("invalid_cursor: offset outside result set");
    }
    let available = set.hits.len() - offset;
    let max_count = available.min(limit).min(budget.limit);
    let compact_live = set.coverage["endpoint"] != "published_working_tree";
    let coverage_line = lines::single_repo_coverage(&set.coverage);
    let render = |count: usize, names: bool, candidate_limit: Option<usize>| -> Result<String> {
        match budget.format {
            OutputFormat::Json => budget.encode(&page_value(
                &set,
                id,
                store.root.as_path(),
                offset,
                count,
                names,
                candidate_limit,
                compact_live,
            )?),
            OutputFormat::Lines => Ok(lines::page_text(
                set.hits[offset..offset + count]
                    .iter()
                    .map(|entry| (None, entry)),
                names,
                &coverage_line,
                lines::next_cursor(id, offset, count, set.hits.len()).as_deref(),
                set.truncated,
            )),
        }
    };

    for names in [false, true] {
        let mut low = 0;
        let mut high = max_count;
        while low < high {
            let count = low + (high - low).div_ceil(2);
            if budget.fits(&render(count, names, None)?) {
                low = count;
            } else {
                high = count - 1;
            }
        }
        if low > 0 {
            let named = render(low, true, None)?;
            if budget.fits(&named) {
                return Ok(named);
            }
            return render(low, names, None);
        }

        // A single ambiguous hit may carry a large candidate list. Trim it by
        // binary search only after the ordinary page shape fails to fit.
        if max_count > 0 && budget.format == OutputFormat::Json {
            let value = page_value(
                &set,
                id,
                store.root.as_path(),
                offset,
                1,
                names,
                None,
                compact_live,
            )?;
            let total = value["hits"][0]["candidates"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            let mut candidate_low = 0;
            let mut candidate_high = total;
            while candidate_low < candidate_high {
                let candidate_limit = candidate_low + (candidate_high - candidate_low).div_ceil(2);
                if budget.fits(&render(1, names, Some(candidate_limit))?) {
                    candidate_low = candidate_limit;
                } else {
                    candidate_high = candidate_limit - 1;
                }
            }
            if total > 0 {
                let text = render(1, names, Some(candidate_low))?;
                if budget.fits(&text) {
                    return Ok(text);
                }
            }
        }
    }

    if available == 0 {
        let text = render(0, false, None)?;
        if budget.fits(&text) {
            return Ok(text);
        }
    }
    bail!("budget_too_small: no hit and pagination envelope fit; increase --budget")
}

fn page_value(
    set: &StoredResultSet,
    id: &str,
    root: &Path,
    offset: usize,
    count: usize,
    names: bool,
    candidate_limit: Option<usize>,
    compact_live: bool,
) -> Result<Value> {
    let mut hits = set.hits[offset..offset + count]
        .iter()
        .map(|entry| compact_entry_for_page(entry, root, None, names, compact_live))
        .collect::<Result<Vec<_>>>()?;
    if let Some(limit) = candidate_limit {
        for hit in &mut hits {
            let Some(candidates) = hit["candidates"].as_array_mut() else {
                continue;
            };
            if candidates.len() > limit {
                let total = candidates.len();
                candidates.truncate(limit);
                hit["candidates_total"] = total.into();
                hit["candidates_truncated"] = true.into();
            }
        }
    }
    let next = (offset + count < set.hits.len()).then(|| format!("{id}@{}", offset + count));
    Ok(serde_json::json!({
        "generation": set.generation,
        "coverage": set.coverage,
        "tokenizer": "o200k_base",
        "hits": hits,
        "next": next,
        "truncated": set.truncated
    }))
}

pub fn more(store: &Store, cursor: &str, limit: usize, budget: &OutputBudget) -> Result<String> {
    let cursor: ResultCursor = cursor.parse()?;
    page(
        store,
        &cursor.set.simple().to_string(),
        cursor.offset,
        limit,
        budget,
    )
}
