use super::*;

pub(super) fn entries(
    history: &History,
    before: Option<&GitOid>,
    after: &GitOid,
    changes: &[FileChange],
    target: Option<&Target>,
) -> Result<(Vec<ResultEntry>, usize, bool)> {
    let mut entries = Vec::with_capacity(changes.len().min(results::MAX_HITS));
    let mut excluded = 0;
    let mut staged_bytes = 0usize;
    let selected_count = changes.iter().filter(|c| selected(c, target)).count();
    let mut truncated = selected_count > results::MAX_HITS;
    for (file_index, change) in changes
        .iter()
        .filter(|c| selected(c, target))
        .take(results::MAX_HITS)
        .enumerate()
    {
        let entry = match comparison::entry(history, before, after, change) {
            Ok(entry) => entry,
            Err(error) => {
                excluded += 1;
                let file = change
                    .after
                    .as_ref()
                    .or(change.before.as_ref())
                    .expect("change file");
                let status = match file.mode.as_str() {
                    "120000" => "symlink_excluded",
                    "160000" => "gitlink_excluded",
                    _ if error.to_string().contains("resource") => "resource_excluded",
                    _ => "object_unavailable",
                };
                entries.push(ResultEntry::Change(crate::results::ChangeEntry {
                    handle: String::new(),
                    name: file.path.clone(),
                    status: status.into(),
                    before: None,
                    after: None,
                    correspondence: "unavailable".into(),
                }));
                continue;
            }
        };
        let entry_bytes = serde_json::to_vec(&entry)?.len();
        let remaining = (16 * 1024 * 1024usize).saturating_sub(staged_bytes) / entry_bytes.max(1);
        if remaining == 0 {
            truncated = true;
            excluded += 1;
            break;
        }
        let previous_count = entries.len();
        if let Some(target) = target.filter(|t| t.symbol.is_some()) {
            entries.extend(symbol_entries(history, entry, target)?);
        } else if target.is_some()
            && entry.before.is_some()
            && entry.after.is_some()
            && change.status != "renamed_exact_blob"
        {
            let old = history
                .repository
                .blob(&entry.before.as_ref().expect("preimage").blob)?;
            let new = history
                .repository
                .blob(&entry.after.as_ref().expect("postimage").blob)?;
            ensure_diff_capacity(&old, &new)?;
            let changes = source_diff::diff_sources(&old, &new);
            truncated |= changes.changes.len()
                > results::MAX_HITS
                    .saturating_sub(entries.len())
                    .min(remaining);
            for change in changes.changes.into_iter().take(
                results::MAX_HITS
                    .saturating_sub(entries.len())
                    .min(remaining),
            ) {
                let mut hunk = entry.clone();
                hunk.before.as_mut().expect("preimage").span = context_span(&old, change.before);
                hunk.after.as_mut().expect("postimage").span = context_span(&new, change.after);
                entries.push(ResultEntry::Change(hunk));
            }
        } else {
            entries.push(ResultEntry::Change(entry));
        }
        staged_bytes += entry_bytes * (entries.len() - previous_count);
        if entries.len() >= results::MAX_HITS {
            truncated |= file_index + 1 < selected_count;
            break;
        }
    }
    Ok((entries, excluded, truncated))
}

fn context_span(bytes: &[u8], span: ByteSpan) -> ByteSpan {
    let previous = bytes[..span.start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    let start = if previous == 0 {
        0
    } else {
        bytes[..previous - 1]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |p| p + 1)
    };
    let end = bytes[span.end..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |p| span.end + p + 1);
    ByteSpan { start, end }
}
