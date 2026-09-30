//! Zero-hit explanations for filtered queries: how many indexed paths each
//! `file:` value matched (with the nearest paths by name when it matched none),
//! a `lang:` value no indexed file has, and filters that only fail together.
//! Counts run in SQL; only values that matched nothing read candidate paths.

use super::{BoundPathFilter, sql_literal};
use anyhow::Result;
use rusqlite::{Connection, params};

const NEAREST_PATHS: usize = 3;
/// Candidate paths read per unmatched value, shortest first.
const NEAREST_CANDIDATES: usize = 400;
/// Leading stem characters a candidate path must contain.
const NEAREST_KEY_CHARS: usize = 4;

/// Coverage text explaining an empty filtered result, or `None` without filters.
pub fn diagnose_empty_filters(
    conn: &Connection,
    filter: &BoundPathFilter,
    language: &str,
) -> Result<Option<String>> {
    if filter.is_empty() && language.is_empty() {
        return Ok(None);
    }
    let count = |clause: &str, with_language: bool| -> Result<usize> {
        let language_clause = if with_language && !language.is_empty() {
            format!(" AND f.language={}", sql_literal(language))
        } else {
            String::new()
        };
        Ok(conn.query_row(
            &format!("SELECT count(*) FROM files f WHERE 1{clause}{language_clause}"),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize)
    };
    let mut parts = Vec::with_capacity(filter.include.len() + 2);
    let mut every_value_matched = true;
    for needle in &filter.include {
        let alone = BoundPathFilter {
            include: vec![needle.clone()],
            exclude: Vec::new(),
        };
        let matched = count(&alone.sql_clause("f.path"), false)?;
        every_value_matched &= matched > 0;
        let noun = if matched == 1 { "path" } else { "paths" };
        let mut part = format!("file:{} matched {matched} indexed {noun}", needle.value());
        if matched == 0 {
            let nearest = nearest_paths(conn, needle.value())?;
            if !nearest.is_empty() {
                part.push_str(&format!(" (nearest: {})", nearest.join(", ")));
            }
        }
        parts.push(part);
    }
    let language_matched = language.is_empty()
        || conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE language=?1)",
            params![language],
            |row| row.get::<_, bool>(0),
        )?;
    if !language_matched {
        parts.push(format!("lang:{language} matched 0 files"));
    }
    if every_value_matched && language_matched {
        let unexcluded = BoundPathFilter {
            include: filter.include.clone(),
            exclude: Vec::new(),
        };
        let filters = filter.include.len() + usize::from(!language.is_empty());
        let combined_unexcluded = count(&unexcluded.sql_clause("f.path"), true)?;
        if combined_unexcluded == 0 && filters > 1 {
            parts.push("filters matched 0 files together".to_owned());
        } else if combined_unexcluded > 0
            && !filter.exclude.is_empty()
            && count(&filter.sql_clause("f.path"), true)? == 0
        {
            parts.push(format!(
                "-file: excluded all {combined_unexcluded} matching files"
            ));
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("; ")))
}

/// Up to three paths or directories named like the value's last segment:
/// names containing it, then names it contains, then names sharing its first
/// characters; nearer the basename and shorter first.
fn nearest_paths(conn: &Connection, value: &str) -> Result<Vec<String>> {
    let last = value
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(value);
    let stem = name_stem(last).to_ascii_lowercase();
    if stem.is_empty() {
        return Ok(Vec::new());
    }
    let key: String = stem.chars().take(NEAREST_KEY_CHARS).collect();
    let mut statement = conn.prepare(
        "SELECT path FROM files WHERE instr(lower(path),?1)>0 ORDER BY length(path),path LIMIT ?2",
    )?;
    let mut rows = statement.query(params![key, NEAREST_CANDIDATES as i64])?;
    let mut ranked: Vec<((u8, usize, usize, usize), String)> =
        Vec::with_capacity(NEAREST_PATHS + 1);
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let Some(((strength, distance, depth), shown)) = related_prefix(&path, &stem, &key) else {
            continue;
        };
        if ranked.iter().any(|(_, known)| *known == shown) {
            continue;
        }
        let rank = (strength, distance, depth, shown.len());
        let position = ranked.partition_point(|(known, _)| *known <= rank);
        if position < NEAREST_PATHS {
            ranked.insert(position, (rank, shown));
            ranked.truncate(NEAREST_PATHS);
        }
    }
    Ok(ranked.into_iter().map(|(_, path)| path).collect())
}

/// The path through its last component related to `stem`, with the relation
/// strength, the name's edit distance from `stem`, and its depth from the basename.
fn related_prefix(path: &str, stem: &str, key: &str) -> Option<((u8, usize, usize), String)> {
    let mut end = path.len();
    for (depth, component) in path.rsplit('/').enumerate() {
        let start = end - component.len();
        let name = name_stem(component).to_ascii_lowercase();
        let strength = if name.contains(stem) {
            0
        } else if name.len() >= NEAREST_KEY_CHARS && stem.contains(&name) {
            1
        } else if name.starts_with(key) {
            2
        } else {
            end = start.saturating_sub(1);
            continue;
        };
        let shown = if depth == 0 {
            path.to_owned()
        } else {
            path[..end + 1].to_owned()
        };
        return Some(((strength, edit_distance(&name, stem), depth), shown));
    }
    None
}

/// Levenshtein distance over bytes; names are short ASCII.
fn edit_distance(left: &str, right: &str) -> usize {
    let right = right.as_bytes();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, &byte) in left.as_bytes().iter().enumerate() {
        current[0] = row + 1;
        for (column, &other) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(byte != other);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn name_stem(name: &str) -> &str {
    name.split_once('.')
        .map_or(name, |(stem, _)| stem)
        .trim_end_matches(['_', '-'])
}
