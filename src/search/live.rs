use super::*;

pub(super) fn live_regex(
    store: &Store,
    query: &Query,
    cache: &std::path::Path,
    hits: &mut Vec<Hit>,
    coverage: &mut serde_json::Value,
    truncated: &mut bool,
) -> Result<()> {
    // Line anchors match at every line, as in grep.
    let regex = regex::bytes::RegexBuilder::new(&query.text)
        .multi_line(true)
        .size_limit(8 * 1024 * 1024)
        .build()?;
    let mut failures = 0usize;
    let mut walk_failures = 0usize;
    let mut checked = 0usize;
    let cache = cache.canonicalize().unwrap_or_else(|_| cache.to_owned());
    let mut walker = ignore::WalkBuilder::new(&store.root);
    walker
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b));
    walker.filter_entry(move |entry| {
        !entry.path().starts_with(&cache)
            && (!entry.file_type().is_some_and(|kind| kind.is_dir())
                || !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "target" | "node_modules" | ".trufflepig")
                ))
    });
    for entry in walker.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                walk_failures += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry.path().strip_prefix(&store.root)?;
        let path = crate::store::encode_path(relative);
        let language = crate::extract::language(&path);
        if !path.starts_with(&query.path)
            || (!query.language.is_empty() && query.language != language)
        {
            continue;
        }
        let source = match source::read_contained(&store.root, relative, source::MAX_READ_BYTES) {
            Ok(bytes) => bytes,
            Err(_) => {
                failures += 1;
                continue;
            }
        };
        if source.contains(&0) {
            failures += 1;
            continue;
        }
        checked += 1;
        let revision = blake3::hash(&source).to_hex().to_string();
        let extraction = if !query.kind.is_empty() && regex.is_match(&source) {
            Some(crate::extract::extract(&path, &source))
        } else {
            None
        };
        // One hit per matching line: later matches on the same line add nothing.
        let mut previous_line = None;
        for matched in regex.find_iter(&source) {
            let (start_line, end_line) = source::line_span(&source, matched.start(), matched.end());
            if previous_line == Some(start_line) {
                continue;
            }
            let definition = extraction.as_ref().and_then(|e| {
                e.definitions
                    .iter()
                    .filter(|d| {
                        d.start <= matched.start()
                            && d.end >= matched.end()
                            && (query.kind.is_empty() || query.kind == d.kind)
                    })
                    .min_by_key(|d| d.end - d.start)
            });
            if !query.kind.is_empty() && definition.is_none() {
                continue;
            }
            if hits.len() >= MAX_HITS {
                *truncated = true;
                break;
            }
            previous_line = Some(start_line);
            hits.push(Hit {
                handle: String::new(),
                path: path.clone(),
                revision: Some(revision.clone()),
                start: matched.start(),
                end: matched.end(),
                start_line,
                end_line,
                name: String::from_utf8_lossy(matched.as_bytes())
                    .chars()
                    .take(96)
                    .collect(),
                kind: definition
                    .map(|d| d.kind.clone())
                    .unwrap_or_else(|| "region".into()),
                container: definition.map(|d| d.name.clone()),
                provenance: Some("live_regex".into()),
                resolution: None,
                candidates: Vec::new(),
                target: None,
                snippet: super::snippets::preview(
                    &source,
                    matched.start(),
                    matched.end(),
                    start_line,
                    &[],
                    "",
                ),
            });
        }
    }
    coverage["live_checked_files"] = checked.into();
    coverage["live_read_failures"] = failures.into();
    coverage["live_walk_failures"] = walk_failures.into();
    Ok(())
}
