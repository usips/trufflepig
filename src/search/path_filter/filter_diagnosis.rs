//! Zero-hit explanations for filtered queries: how many indexed paths each
//! `file:` value matched (with the nearest paths by name when it matched none),
//! a `lang:` value no indexed file has, and filters that only fail together.

use super::{BoundPathFilter, PathFilter};
use anyhow::Result;
use rusqlite::Connection;

const NEAREST_PATHS: usize = 3;

/// Coverage text explaining an empty filtered result, or `None` without filters.
pub fn diagnose_empty_filters(
    conn: &Connection,
    filter: &PathFilter,
    language: &str,
) -> Result<Option<String>> {
    if filter.is_empty() && language.is_empty() {
        return Ok(None);
    }
    let bound = filter.bind(conn)?;
    let unexcluded = PathFilter {
        include: filter.include.clone(),
        exclude: Vec::new(),
    }
    .bind(conn)?;
    let mut values = filter
        .include
        .iter()
        .map(|value| {
            Ok(ValueTally {
                value,
                filter: PathFilter::prefix(value).bind(conn)?,
                matched: 0,
                nearest: NearestPaths::new(value),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let (mut language_matched, mut combined, mut combined_unexcluded) = (0usize, 0usize, 0usize);
    let mut statement = conn.prepare("SELECT path,language FROM files")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let language_matches = language.is_empty() || row.get_ref(1)?.as_str()? == language;
        language_matched += usize::from(language_matches);
        for tally in &mut values {
            if tally.filter.matches(&path) {
                tally.matched += 1;
            } else if tally.matched == 0 {
                tally.nearest.consider(&path);
            }
        }
        if language_matches && unexcluded.matches(&path) {
            combined_unexcluded += 1;
            combined += usize::from(bound.matches(&path));
        }
    }
    let mut parts = Vec::with_capacity(values.len() + 2);
    for tally in &values {
        let noun = if tally.matched == 1 { "path" } else { "paths" };
        let mut part = format!(
            "file:{} matched {} indexed {noun}",
            tally.value, tally.matched
        );
        if tally.matched == 0 && !tally.nearest.paths.is_empty() {
            let nearest: Vec<_> = tally
                .nearest
                .paths
                .iter()
                .map(|(_, path)| path.as_str())
                .collect();
            part.push_str(&format!(" (nearest: {})", nearest.join(", ")));
        }
        parts.push(part);
    }
    if !language.is_empty() && language_matched == 0 {
        parts.push(format!("lang:{language} matched 0 files"));
    }
    let individually_matched = values.iter().all(|tally| tally.matched > 0)
        && (language.is_empty() || language_matched > 0);
    if individually_matched
        && combined_unexcluded == 0
        && values.len() + usize::from(!language.is_empty()) > 1
    {
        parts.push("filters matched 0 files together".to_owned());
    } else if combined_unexcluded > 0 && combined == 0 {
        parts.push(format!(
            "-file: excluded all {combined_unexcluded} matching files"
        ));
    }
    Ok((!parts.is_empty()).then(|| parts.join("; ")))
}

struct ValueTally<'a> {
    value: &'a str,
    filter: BoundPathFilter,
    matched: usize,
    nearest: NearestPaths,
}

/// Paths or directories whose name shares the value's last segment, best first.
struct NearestPaths {
    stem: String,
    /// `((depth from the basename, length), shown path)`, sorted and deduplicated.
    paths: Vec<((usize, usize), String)>,
}

impl NearestPaths {
    fn new(value: &str) -> Self {
        let last = value
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or(value);
        Self {
            stem: name_stem(last).to_lowercase(),
            paths: Vec::with_capacity(NEAREST_PATHS + 1),
        }
    }

    fn consider(&mut self, path: &str) {
        if self.stem.is_empty() {
            return;
        }
        let mut end = path.len();
        for (depth, component) in path.rsplit('/').enumerate() {
            let start = end - component.len();
            let stem = name_stem(component).to_lowercase();
            let related = stem.contains(&self.stem)
                || (stem.chars().count() >= 4 && self.stem.contains(&stem));
            if related {
                let shown = if depth == 0 {
                    path.to_owned()
                } else {
                    path[..end + 1].to_owned()
                };
                let key = (depth, shown.len());
                if !self.paths.iter().any(|(_, known)| *known == shown) {
                    let position = self.paths.partition_point(|(known, _)| *known <= key);
                    if position < NEAREST_PATHS {
                        self.paths.insert(position, (key, shown));
                        self.paths.truncate(NEAREST_PATHS);
                    }
                }
                return;
            }
            end = start.saturating_sub(1);
        }
    }
}

fn name_stem(name: &str) -> &str {
    name.split_once('.')
        .map_or(name, |(stem, _)| stem)
        .trim_end_matches(['_', '-'])
}
