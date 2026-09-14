use super::*;

pub(super) fn symbol_entries(
    history: &History,
    entry: crate::results::ChangeEntry,
    target: &Target,
) -> Result<Vec<ResultEntry>> {
    let (entry, _, _) = symbol_correspondence(history, entry, target)?;
    Ok(entry.into_iter().map(ResultEntry::Change).collect())
}

pub(super) fn symbol_correspondence(
    history: &History,
    mut entry: crate::results::ChangeEntry,
    target: &Target,
) -> Result<(
    Option<crate::results::ChangeEntry>,
    Option<ByteSpan>,
    Option<String>,
)> {
    let old = entry
        .before
        .as_ref()
        .map(|s| history.repository.blob(&s.blob))
        .transpose()?
        .unwrap_or_default();
    let new = entry
        .after
        .as_ref()
        .map(|s| history.repository.blob(&s.blob))
        .transpose()?
        .unwrap_or_default();
    let before = crate::extract::extract(
        entry
            .before
            .as_ref()
            .map(|s| s.path.as_str())
            .unwrap_or(&target.path),
        &old,
    );
    let after = crate::extract::extract(
        entry
            .after
            .as_ref()
            .map(|s| s.path.as_str())
            .unwrap_or(&target.path),
        &new,
    );
    if let Some(revision) = &target.revision {
        anyhow::ensure!(
            blake3::hash(&new).to_hex().as_str() == revision,
            "stale_source: symbol selector does not identify this historical postimage"
        );
    }
    let selected=after.definitions.iter().position(|d|Some(&d.name)==target.symbol.as_ref() && target.span.is_some_and(|s|s.start==d.start && s.end==d.end)).context("uncertain_symbol_correspondence: selected occurrence is absent from historical postimage")?;
    ensure_diff_capacity(&old, &new)?;
    let differences = source_diff::diff_sources(&old, &new);
    let correspondences =
        source_diff::correspond_declarations(&old, &new, &before, &after, &differences);
    let row = correspondences
        .iter()
        .find(|row| row.after == Some(selected))
        .context("uncertain_symbol_correspondence: selected occurrence has no correspondence")?;
    let preimage = row.before.map(|i| &before.definitions[i]);
    let prior_span = preimage.map(|d| ByteSpan {
        start: d.start,
        end: d.end,
    });
    let prior_revision = preimage.map(|_| blake3::hash(&old).to_hex().to_string());
    if row.relation == source_diff::DeclarationRelation::Unchanged {
        return Ok((None, prior_span, prior_revision));
    }
    entry.name = target.symbol.clone().expect("symbol target");
    entry.correspondence = serde_json::to_value(row.relation)?
        .as_str()
        .unwrap_or("uncertain")
        .into();
    entry.status = entry.correspondence.clone();
    match prior_span {
        Some(span) => {
            if let Some(source) = &mut entry.before {
                source.span = span;
            }
        }
        None => entry.before = None,
    }
    if let Some(source) = &mut entry.after {
        let d = &after.definitions[selected];
        source.span = ByteSpan {
            start: d.start,
            end: d.end,
        };
    }
    Ok((Some(entry), prior_span, prior_revision))
}
