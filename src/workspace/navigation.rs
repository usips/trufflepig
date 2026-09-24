//! Owner routing verifies recorded roots and source identities before every follow-up.
use super::{
    WorkspaceConfig, member_cache, member_options,
    result_cache::{MemberSnapshot, OwnedEntry, WorkspaceResults, WorkspaceSet},
    selected_member,
};
use crate::{
    cli::Arguments,
    identity::ResultHandle,
    output::OutputBudget,
    results::{self, ResultEntry},
    source::{self, SourceSide, acquisition},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

fn address(target: &str) -> Result<(String, Option<SourceSide>, Option<usize>)> {
    if let Some(cursor) = target.strip_prefix("read:") {
        let (handle, offset) = cursor
            .rsplit_once('@')
            .context("invalid_cursor: expected source byte offset")?;
        let (handle, side) = match handle.rsplit_once(':') {
            Some((handle, side @ ("before" | "after"))) => (handle, Some(SourceSide::parse(side)?)),
            _ => (handle, None),
        };
        let parsed: ResultHandle = handle.parse()?;
        return Ok((
            parsed.to_string(),
            side,
            Some(offset.parse().context("invalid_cursor: byte offset")?),
        ));
    }
    Ok((target.parse::<ResultHandle>()?.to_string(), None, None))
}
fn verify_selection(owner: &MemberSnapshot, options: &Arguments) -> Result<()> {
    ensure!(
        options
            .member
            .as_ref()
            .is_none_or(|name| name == &owner.name),
        "invalid_member: selector conflicts with immutable owner"
    );
    Ok(())
}
pub(super) fn read(
    config: &WorkspaceConfig,
    results: &WorkspaceResults,
    options: &Arguments,
    budget: &OutputBudget,
) -> Result<String> {
    let target = options
        .words
        .get(1)
        .context("usage: show TARGET | ctx HANDLE")?;
    let side = options.side.as_deref().map(SourceSide::parse).transpose()?;
    let is_context = options.words[0] == "ctx";
    let addressed = target.starts_with("read:")
        || target.parse::<ResultHandle>().is_ok()
        || target
            .split(':')
            .next()
            .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok());
    if addressed {
        let (handle, recorded_side, offset) = address(target)?;
        ensure!(
            offset.is_none() || side.is_none() || side == recorded_side,
            "invalid_side: continuation side cannot change"
        );
        let (set, index) = results.entry(&handle)?;
        let hit = &set.hits[index];
        let owner = &set.owners[hit.owner];
        verify_selection(owner, options)?;
        let store = owner.open(config)?;
        let metadata = owner.metadata(&set.workspace);
        if is_context {
            ensure!(
                offset.is_none() && side.is_none(),
                "invalid_handle: context requires a live result handle"
            );
            let ResultEntry::LiveSource(hit) = &hit.entry else {
                anyhow::bail!("historical_result: historical entries are invalid for live context");
            };
            return crate::search::context_entry(
                &store,
                owner.generation,
                hit.clone(),
                budget,
                &metadata,
            );
        }
        let mut source =
            acquisition::acquire_entry(&store, &handle, hit.entry.clone(), side.or(recorded_side))?;
        if let Some(offset) = offset {
            ensure!(
                offset >= source.span.start && offset <= source.span.end,
                "invalid_cursor: offset outside original source range"
            );
            source.span.start = offset;
        }
        return source::render_owned(source, budget, &metadata);
    }
    ensure!(
        !is_context,
        "invalid_handle: context requires a live result handle"
    );
    let member = selected_member(config, options)?;
    member.verify_identity()?;
    let cache = member_cache(&member, options.cache.as_deref())?;
    let store = Store::open(&member.root, &cache)?;
    let mut source = acquisition::acquire(&store, target, side)?;
    let (_, entry) = results::entry(&store, &source.handle)?;
    let owner = MemberSnapshot::capture(&member, &store, &cache, store.generation()?)?;
    let metadata = owner.metadata(&config.name);
    let id = results.save(WorkspaceSet {
        workspace: config.name.clone(),
        home: Some(member.name().to_owned()),
        owners: vec![owner],
        coverage: vec![],
        hits: vec![OwnedEntry {
            owner: 0,
            member_rank: 1,
            entry,
        }],
        truncated: false,
        scope: None,
    })?;
    source.handle = format!("{id}:1");
    source::render_owned(source, budget, &metadata)
}
pub(super) fn history(
    config: &WorkspaceConfig,
    workspace_results: &WorkspaceResults,
    options: &Arguments,
    budget: &OutputBudget,
) -> Result<String> {
    let selector = if matches!(options.words[0].as_str(), "hist" | "blame") {
        options.words.get(1)
    } else {
        options.target.as_ref().or_else(|| options.words.get(2))
    };
    let retained = selector
        .filter(|s| s.parse::<ResultHandle>().is_ok())
        .map(|s| workspace_results.entry(s))
        .transpose()?;
    let member = if let Some((set, index)) = &retained {
        let owner = &set.owners[set.hits[*index].owner];
        verify_selection(owner, options)?;
        let _ = owner.open(config)?;
        owner.member_root(config)?
    } else {
        selected_member(config, options)?
    };
    member.verify_identity()?;
    let cache = member_cache(&member, options.cache.as_deref())?;
    let store = Store::open(&member.root, &cache)?;
    let mut local = member_options(options, &member)?;
    local.budget = 1_000_000;
    if let Some((set, index)) = &retained {
        let owned = &set.hits[*index];
        let owner = &set.owners[owned.owner];
        let id = results::save_entries(
            &store,
            owner.generation,
            json!({}),
            vec![owned.entry.clone()],
            false,
        )?;
        let replacement = format!("{id}:1");
        for word in &mut local.words {
            if Some(&*word) == selector {
                *word = replacement.clone();
            }
        }
        if local.target.as_ref() == selector {
            local.target = Some(replacement);
        }
    }
    // Execute against this owner's store; result sets are imported before final rendering.
    let output = crate::cli::local(&member.root, &cache, &local, !options.no_daemon)?;
    let mut value: Value = serde_json::from_str(&output)?;
    let handle = value["hits"]
        .as_array()
        .and_then(|hits| hits.first())
        .and_then(|h| h["handle"].as_str())
        .map(str::to_owned);
    if let Some(handle) = handle {
        let parsed: ResultHandle = handle.parse()?;
        let original = parsed.set.simple().to_string();
        let set = results::load_entries(&store, &original)?;
        let owner = MemberSnapshot::capture(&member, &store, &cache, set.generation)?;
        let entries = set
            .hits
            .into_iter()
            .enumerate()
            .map(|(rank, entry)| OwnedEntry {
                owner: 0,
                member_rank: rank + 1,
                entry,
            })
            .collect();
        let id = workspace_results.save(WorkspaceSet {workspace:config.name.clone(),home:Some(member.name().to_owned()),owners:vec![owner],coverage:vec![json!({"member":member.name(),"state":"searched","generation":set.generation,"detail":set.coverage})],hits:entries,truncated:set.truncated,scope:None})?;
        rewrite_handles(&mut value, &original, &id);
        if value.get("hunks").is_none() {
            return workspace_results.page(&id, 0, options.limit, budget);
        }
    }
    if let Some(hits) = value["hits"].as_array_mut() {
        for hit in hits {
            hit["member"] = member.name().into();
        }
    }
    value["workspace"] = config.name.clone().into();
    value["member"] = member.name().into();
    value["repository"] = crate::store::encode_path(&member.root).into();
    render_owned_value(value, budget)
}
fn rewrite_handles(value: &mut Value, old: &str, new: &str) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "handle" | "next") {
                    if let Some(s) = value.as_str() {
                        *value = s.replace(old, new).into();
                    }
                } else {
                    rewrite_handles(value, old, new);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                rewrite_handles(value, old, new);
            }
        }
        _ => {}
    }
}
fn render_owned_value(mut value: Value, budget: &OutputBudget) -> Result<String> {
    loop {
        let output = budget.encode(&value)?;
        if budget.fits(&output) {
            return Ok(output);
        }
        if let Some(hunks) = value["hunks"].as_array_mut() {
            if hunks.pop().is_some() {
                value["truncated"] = true.into();
                value["hunks_truncated"] = true.into();
                continue;
            }
        }
        if let Some(hits) = value["hits"].as_array_mut() {
            if hits.len() > 1 {
                let removed = hits.pop().expect("nonempty");
                if let Some(handle) = removed["handle"].as_str() {
                    let parsed: ResultHandle = handle.parse()?;
                    value["next"] =
                        format!("{}@{}", parsed.set.simple(), parsed.ordinal - 1).into();
                }
                value["truncated"] = true.into();
                continue;
            }
        }
        anyhow::bail!("budget_too_small: owner response and provenance do not fit");
    }
}
