//! Follows an import row (`use a::b as c`, `pub use a::b`) through at most
//! `IMPORT_HOPS` re-exports to the declaration its path names, ignoring the path
//! filter that selected the import. `crate::`, `self::` and `super::` resolve
//! against the importing module (file path plus enclosing inline `mod`s).
use super::{
    BoundPathFilter, Query,
    declarations::{InvocationDirectory, qualified_hits, rank_declarations, rank_key},
    qualified_name::{QualifiedName, crate_root_segments, import_spelling, module_segments},
};
use crate::{results::Hit, store::Store};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

/// Import rows followed before a trail stops as unresolved.
const IMPORT_HOPS: usize = 4;

/// One import passed through: `site` is `PATH:LINE`, `path` its spelled path.
#[derive(Debug)]
pub struct ImportHop {
    pub site: String,
    pub path: String,
    pub reexport: bool,
}

/// The imports followed from the selected row, the declaration they reach (if
/// indexed), and other declarations tied with it at the same rank.
#[derive(Debug)]
pub struct ImportTrail {
    pub hops: Vec<ImportHop>,
    pub declaration: Option<Hit>,
    pub ties: Vec<Hit>,
}

pub fn trace_import(
    store: &Store,
    import: &Hit,
    query: &Query,
    origin: &InvocationDirectory,
) -> Result<Option<ImportTrail>> {
    let mut hops = Vec::with_capacity(IMPORT_HOPS);
    let mut visited = HashSet::with_capacity(IMPORT_HOPS);
    let mut current = import.clone();
    while hops.len() < IMPORT_HOPS && visited.insert((current.path.clone(), current.start)) {
        let Some((hop, original)) = read_hop(store, &current)? else {
            break;
        };
        hops.push(hop);
        let mut hits = qualified_hits(
            store,
            &original,
            &BoundPathFilter::default(),
            &query.language,
            "",
        )?
        .hits;
        rank_declarations(&mut hits, origin);
        if let Some(best) = hits.iter().position(|hit| hit.kind != "import") {
            let declaration = hits.remove(best);
            let key = rank_key(&declaration, origin);
            let ties = hits
                .into_iter()
                .filter(|hit| hit.kind != "import" && rank_key(hit, origin) == key)
                .collect();
            return Ok(Some(ImportTrail {
                hops,
                declaration: Some(declaration),
                ties,
            }));
        }
        match hits
            .into_iter()
            .find(|hit| !visited.contains(&(hit.path.clone(), hit.start)))
        {
            Some(next) => current = next,
            None => break,
        }
    }
    Ok((!hops.is_empty()).then_some(ImportTrail {
        hops,
        declaration: None,
        ties: Vec::new(),
    }))
}

/// Reads one import row's spelling and resolves its path in the importing module.
fn read_hop(store: &Store, import: &Hit) -> Result<Option<(ImportHop, QualifiedName)>> {
    let Some(revision) = import.revision.as_deref() else {
        return Ok(None);
    };
    let bytes: Option<Vec<u8>> = store
        .conn
        .query_row(
            "SELECT bytes FROM contents WHERE revision=?1",
            [revision],
            |row| row.get(0),
        )
        .optional()?;
    let Some(bytes) = bytes.filter(|bytes| import.end <= bytes.len()) else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&bytes[import.start..import.end]);
    let Some((path, _alias)) = import_spelling(&text) else {
        return Ok(None);
    };
    let mut module = module_segments(&import.path);
    let mut statement = store.conn.prepare(
        "SELECT d.name FROM definitions d JOIN files f ON f.id=d.file_id
         WHERE f.path=?1 AND d.kind='module' AND d.start<=?2 AND d.end>=?3
           AND NOT (d.start=0 AND d.name=f.path)
         ORDER BY d.start,d.id",
    )?;
    for name in statement.query_map(
        params![import.path, import.start as i64, import.end as i64],
        |row| row.get::<_, String>(0),
    )? {
        module.push(name?);
    }
    let original = QualifiedName::parse_in(&path, &module, &crate_root_segments(&import.path));
    let hop = ImportHop {
        site: format!("{}:{}", import.path, import.start_line),
        reexport: is_reexport(&bytes[..import.start]),
        path,
    };
    Ok(Some((hop, original)))
}

/// Whether the `use` statement enclosing an import starts with `pub`.
fn is_reexport(before: &[u8]) -> bool {
    let before = String::from_utf8_lossy(before);
    let Some(keyword) = before.rfind("use ") else {
        return false;
    };
    let statement = before[..keyword]
        .rfind([';', '}', '{', '\n'])
        .map_or(0, |boundary| boundary + 1);
    before[statement..keyword].trim_start().starts_with("pub")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexport_detection_reads_the_enclosing_use_statement() {
        assert!(is_reexport(b"pub use crate::a::"));
        assert!(is_reexport(b"x();\npub(crate) use a::{\n    b,\n    "));
        assert!(!is_reexport(b"use a::"));
        assert!(!is_reexport(b"fn pubby() {}\nuse a::"));
    }
}
