//! `map PATH`. A file lists its types, functions, methods and constants in
//! source order, then folded rows (fields, variants, properties, `mod x;`); the
//! whole-file module row and `extern` block signatures are omitted. A directory
//! prefix lists one row per file naming its top-level items. A path with no
//! indexed file fails with `no_indexed_path` and up to three nearest paths.
use crate::{
    results::{Hit, MAX_HITS, ResultSet},
    search::{hit_row, snippets},
    store::Store,
};
use anyhow::Result;
use rusqlite::params;

/// Longest item-name list a directory row carries before summarizing as `+N`.
const DIRECTORY_ROW_NAMES: usize = 90;

/// Outlines `path`. A path with no indexed file yields an empty set whose
/// coverage carries the `no_indexed_path` message (see [`map_miss`]).
pub fn map(store: &Store, path: &str) -> Result<ResultSet> {
    let snapshot = store.conn.unchecked_transaction()?;
    let generation = store.generation()?;
    let mut coverage = serde_json::to_value(store.coverage()?)?;
    let is_file: bool = store.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1)",
        [path],
        |row| row.get(0),
    )?;
    let mut hits = if is_file {
        file_outline(store, path)?
    } else {
        directory_outline(store, path)?
    };
    if hits.is_empty() && !is_file {
        let nearest = nearest_paths(store, path)?;
        coverage[MAP_MISS_KEY] = format!(
            "no_indexed_path: no indexed file under `{path}`; nearest: {}",
            if nearest.is_empty() {
                "none".to_owned()
            } else {
                nearest.join(", ")
            }
        )
        .into();
    }
    let truncated = hits.len() > MAX_HITS;
    hits.truncate(MAX_HITS);
    snippets::attach(store, &[], &mut hits)?;
    snapshot.commit()?;
    Ok(ResultSet {
        generation,
        coverage,
        truncated,
        hits,
    })
}

const MAP_MISS_KEY: &str = "no_indexed_path";

/// The `no_indexed_path` error text of a `map` result whose path matched no file.
pub fn map_miss(set: &ResultSet) -> Option<&str> {
    set.coverage[MAP_MISS_KEY].as_str()
}

fn file_outline(store: &Store, path: &str) -> Result<Vec<Hit>> {
    let mut stmt = store.conn.prepare(
        "SELECT f.path,f.revision,d.start,d.end,d.name,d.kind,d.container,'structural_map',c.bytes
         FROM definitions d
         JOIN files f ON f.id=d.file_id
         JOIN contents c ON c.revision=f.revision
         WHERE f.path=?1
           AND d.kind IN ('addon','module','namespace','struct','class','trait','type','enum',
                          'impl','interface','record','delegate','xenforo_class_extension',
                          'function','method','constructor','property','field','event',
                          'constant','enum_case','variant','macro')
           AND NOT (d.kind='module' AND d.start=0 AND d.name=f.path AND d.container IS NULL)
           AND coalesce(d.container,'')<>?3
         ORDER BY CASE WHEN d.kind IN ('field','enum_case','variant','property','event','module')
                       THEN 1 ELSE 0 END,
                  d.start,d.id
         LIMIT ?2",
    )?;
    let hits = stmt
        .query_map(
            params![
                path,
                (MAX_HITS + 1) as i64,
                crate::extract::FOREIGN_ITEM_CONTAINER
            ],
            hit_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(hits)
}

/// One `file` row per indexed file under `prefix`; `name` lists types, then
/// top-level functions, constants and macros, then `mod` declarations.
fn directory_outline(store: &Store, prefix: &str) -> Result<Vec<Hit>> {
    let mut stmt = store.conn.prepare(
        "WITH scoped AS (
             SELECT f.id,f.path,f.revision,length(c.bytes) AS size,
                    1+length(CAST(c.bytes AS TEXT))
                     -length(replace(CAST(c.bytes AS TEXT),char(10),''))
                     -(substr(c.bytes,-1)=x'0a') AS last_line
             FROM files f JOIN contents c ON c.revision=f.revision
             WHERE substr(f.path,1,length(?1))=?1
             ORDER BY f.path
             LIMIT ?2
         )
         SELECT s.path,s.revision,s.size,s.last_line,d.name
         FROM scoped s
         LEFT JOIN definitions d ON d.file_id=s.id AND (
             (d.kind IN ('addon','struct','class','trait','type','enum','interface','record',
                         'delegate','xenforo_class_extension')
              AND coalesce(d.container,'')<>?3)
             OR (d.container IS NULL AND d.kind IN ('function','constant','macro'))
             OR (d.container IS NULL AND d.kind='module' AND NOT (d.start=0 AND d.name=s.path)))
         ORDER BY s.path,
                  CASE WHEN d.kind='module' THEN 2
                       WHEN d.kind IN ('function','constant','macro') THEN 1 ELSE 0 END,
                  d.start,d.id",
    )?;
    let mut rows = stmt.query(params![
        prefix,
        (MAX_HITS + 1) as i64,
        crate::extract::FOREIGN_ITEM_CONTAINER
    ])?;
    let mut hits: Vec<Hit> = Vec::with_capacity(64);
    let mut hidden = 0usize;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        if hits.last().is_none_or(|hit| hit.path != path) {
            finish_directory_row(hits.last_mut(), hidden);
            hidden = 0;
            let size = row.get::<_, i64>(2)? as usize;
            hits.push(Hit {
                handle: String::new(),
                path: path.clone(),
                revision: row.get(1)?,
                start: 0,
                end: size,
                start_line: 1,
                end_line: row.get::<_, i64>(3)?.max(1) as usize,
                name: String::new(),
                kind: "file".into(),
                container: None,
                provenance: Some("structural_map".into()),
                resolution: None,
                candidates: Vec::new(),
                target: None,
                repeats: None,
                snippet: None,
            });
        }
        let Some(item) = row.get::<_, Option<String>>(4)? else {
            continue;
        };
        let row_hit = hits.last_mut().expect("file row");
        let separator = if row_hit.name.is_empty() { 0 } else { 2 };
        if hidden == 0 && row_hit.name.len() + separator + item.len() <= DIRECTORY_ROW_NAMES {
            if separator > 0 {
                row_hit.name.push_str(", ");
            }
            row_hit.name.push_str(&item);
        } else {
            hidden += 1;
        }
    }
    finish_directory_row(hits.last_mut(), hidden);
    Ok(hits)
}

fn finish_directory_row(hit: Option<&mut Hit>, hidden: usize) {
    if let Some(hit) = hit
        && hidden > 0
    {
        hit.name.push_str(&format!(" +{hidden}"));
    }
}

/// Up to three indexed paths resembling a missing one: same basename (most
/// shared trailing components first), else the entries under the deepest
/// existing ancestor whose names are closest to the missing component.
fn nearest_paths(store: &Store, path: &str) -> Result<Vec<String>> {
    let trimmed = path.trim_matches('/');
    let basename = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let mut nearest = Vec::with_capacity(3);
    if !basename.is_empty() {
        let mut stmt = store.conn.prepare(
            "SELECT path FROM files
             WHERE path=?1 OR substr(path,-length(?1)-1)='/'||?1
                OR instr(path,'/'||?1||'/')>0 OR substr(path,1,length(?1)+1)=?1||'/'
             ORDER BY path LIMIT 200",
        )?;
        let mut found: Vec<String> = stmt
            .query_map([basename], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|candidate| through_component(&candidate, basename))
            .collect();
        found.dedup();
        found.sort_by_key(|candidate| std::cmp::Reverse(shared_suffix(candidate, trimmed)));
        for candidate in found {
            if !nearest.contains(&candidate) && nearest.len() < 3 {
                nearest.push(candidate);
            }
        }
    }
    let mut ancestor = trimmed;
    while nearest.is_empty() && !ancestor.is_empty() {
        let missing = ancestor.rsplit('/').next().unwrap_or(ancestor);
        ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
        let prefix = if ancestor.is_empty() {
            String::new()
        } else {
            format!("{ancestor}/")
        };
        let mut stmt = store.conn.prepare(
            "SELECT path FROM files WHERE substr(path,1,length(?1))=?1 ORDER BY path LIMIT 5000",
        )?;
        let mut entries: Vec<String> = Vec::with_capacity(64);
        for child in stmt.query_map([&prefix], |row| row.get::<_, String>(0))? {
            let child = child?;
            let entry = match child[prefix.len()..].split_once('/') {
                Some((directory, _)) => format!("{prefix}{directory}/"),
                None => child,
            };
            if entries.last() != Some(&entry) {
                entries.push(entry);
            }
        }
        entries.sort_by_cached_key(|entry| {
            let name = entry[prefix.len()..].trim_end_matches('/');
            let stem = name.split('.').next().unwrap_or(name);
            edit_distance(stem, missing)
        });
        entries.dedup();
        nearest.extend(entries.into_iter().take(3));
    }
    Ok(nearest)
}

/// `candidate` cut after the component equal to `component` (a directory match
/// becomes `dir/`), or unchanged when that component is the file itself.
fn through_component(candidate: &str, component: &str) -> String {
    let mut end = 0;
    for part in candidate.split('/') {
        end += part.len();
        if part == component {
            return if end == candidate.len() {
                candidate.to_owned()
            } else {
                format!("{}/", &candidate[..end])
            };
        }
        end += 1;
    }
    candidate.to_owned()
}

/// Levenshtein distance over characters; names here are short.
fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, left_char) in left.chars().enumerate() {
        current[0] = row + 1;
        for (column, right_char) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_char != *right_char);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn shared_suffix(candidate: &str, wanted: &str) -> usize {
    candidate
        .trim_end_matches('/')
        .rsplit('/')
        .zip(wanted.rsplit('/'))
        .take_while(|(left, right)| left == right)
        .count()
}

#[cfg(test)]
mod tests;
