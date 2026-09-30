//! Exact-name declaration lookup behind `sym:` and `show 'sym:'`. Rows rank by
//! kind tier, then non-test before test sites, then nearest to the invocation
//! directory, then path; `A::b` names are filtered by [`QualifiedName`].
use super::{
    LaneHits, Query, cap_hits, hit_row,
    qualified_name::{QualifiedName, import_spelling},
};
use crate::{
    results::{Hit, MAX_HITS},
    store::Store,
};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::{cmp::Reverse, path::Path};

/// Root-relative directory a command was invoked from (empty at the root).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InvocationDirectory(String);

impl InvocationDirectory {
    pub fn root() -> Self {
        Self::default()
    }

    /// `invocation` relative to `root`; a directory outside `root` counts as the root.
    pub fn within(root: &Path, invocation: &Path) -> Self {
        invocation
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .map(|relative| Self(relative.trim_matches('/').to_owned()))
            .unwrap_or_default()
    }

    /// Leading directory components shared with `path`'s directory.
    fn shared_depth(&self, path: &str) -> usize {
        if self.0.is_empty() {
            return 0;
        }
        let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
        self.0
            .split('/')
            .zip(directory.split('/'))
            .take_while(|(left, right)| left == right)
            .count()
    }
}

/// Declarations a reader usually means by a name rank before modules, modules
/// before members, and members before locals and imports that merely share it.
pub(super) fn declaration_rank(kind: &str) -> u8 {
    match kind {
        "module" => 1,
        "variant" | "field" | "constructor" => 2,
        "variable" | "parameter" | "import" => 3,
        _ => 0,
    }
}

/// Test files and fixtures by path convention (`tests/`, `tests.rs`, `*_test.*`,
/// `test_*`, `*.spec.*`, `*.test.*`, `fixtures/`).
pub(crate) fn is_test_path(path: &str) -> bool {
    let (directories, file) = path.rsplit_once('/').unwrap_or(("", path));
    let stem = file.split('.').next().unwrap_or(file);
    directories.split('/').any(|directory| {
        matches!(
            directory,
            "tests" | "test" | "__tests__" | "fixtures" | "testdata"
        )
    }) || matches!(stem, "tests" | "test")
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || file.contains(".spec.")
        || file.contains(".test.")
}

/// Whether a definition sits in a test file or an inline `mod tests`.
fn is_test_site(hit: &Hit) -> bool {
    is_test_path(&hit.path)
        || hit
            .container
            .as_deref()
            .is_some_and(|container| container == "tests" || container.starts_with("tests::"))
}

/// Stable ranking: kind tier, non-test first, nearest directory, then SQL path order.
pub(crate) fn rank_declarations(hits: &mut [Hit], origin: &InvocationDirectory) {
    hits.sort_by_key(|hit| {
        (
            declaration_rank(&hit.kind),
            is_test_site(hit),
            Reverse(origin.shared_depth(&hit.path)),
        )
    });
}

/// Definitions named by `query.text` (`name` or `Qualifier::name`) under the
/// query's filters, in path order. Qualified names keep only the closest tier.
pub(super) fn declaration_hits(store: &Store, query: &Query) -> Result<LaneHits> {
    let name = QualifiedName::parse(&query.text);
    let owner_hint = name.qualifier.last().map_or("", String::as_str);
    let mut statement = store.conn.prepare(
        "SELECT f.path,f.revision,d.start,d.end,d.name,d.kind,d.container,
                'exact_identifier',c.bytes
         FROM definitions d
         JOIN files f ON f.id=d.file_id
         JOIN contents c ON c.revision=f.revision
         WHERE d.name=?1
           AND substr(f.path,1,length(?2))=?2
           AND (?3='' OR f.language=?3)
           AND (?4='' OR d.kind=?4)
           AND (?6='' OR instr(coalesce(d.container,'')||'/'||replace(f.path,'-','_'),?6)>0)
         ORDER BY f.path,d.start,d.end,d.id
         LIMIT ?5",
    )?;
    let mut hits = statement
        .query_map(
            params![
                name.name,
                query.path,
                query.language,
                query.kind,
                (MAX_HITS + 1) as i64,
                owner_hint
            ],
            hit_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if name.is_qualified() {
        let tiers: Vec<_> = hits
            .iter()
            .map(|hit| name.matches(&hit.path, hit.container.as_deref()))
            .collect();
        let best = tiers.iter().flatten().min().copied();
        let mut tier = tiers.into_iter();
        hits.retain(|_| {
            tier.next()
                .flatten()
                .is_some_and(|found| Some(found) == best)
        });
    }
    Ok(cap_hits(&mut hits, MAX_HITS))
}

/// Where an import row leads: its spelled path, whether it is a `pub` re-export,
/// and the declaration of the original name when one is indexed.
#[derive(Debug)]
pub struct ImportTrail {
    pub site: String,
    pub path: String,
    pub reexport: bool,
    pub declaration: Option<Hit>,
}

/// Follows an import row (`use a::b as c`, `pub use a::b`) to the declaration
/// its path names, ignoring the path filter that selected the import.
pub fn trace_import(
    store: &Store,
    import: &Hit,
    query: &Query,
    origin: &InvocationDirectory,
) -> Result<Option<ImportTrail>> {
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
    let original = Query {
        text: path.clone(),
        language: query.language.clone(),
        exact: true,
        ..Query::default()
    };
    let mut hits = declaration_hits(store, &original)?.hits;
    hits.retain(|hit| hit.kind != "import");
    rank_declarations(&mut hits, origin);
    Ok(Some(ImportTrail {
        site: format!("{}:{}", import.path, import.start_line),
        reexport: is_reexport(&bytes[..import.start]),
        path,
        declaration: hits.into_iter().next(),
    }))
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
mod tests;
