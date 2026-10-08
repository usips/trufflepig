//! Exact-name declaration lookup behind `sym:` and `show 'sym:'`. Rows rank by
//! kind tier, then non-test before test sites, then nearest to the invocation
//! directory, then path; `A::b` names are filtered by [`QualifiedName`].
use super::{
    BoundPathFilter, LaneHits, Query, hit_row, path_class::is_test_path,
    qualified_name::QualifiedName,
};
use crate::{
    results::{Hit, MAX_HITS},
    store::Store,
};
use anyhow::Result;
use rusqlite::params;
use std::{
    cmp::Reverse,
    path::{Path, PathBuf},
};

/// Root-relative directory a command was invoked from (empty at the root).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InvocationDirectory(String);

impl InvocationDirectory {
    pub fn root() -> Self {
        Self::default()
    }

    /// `invocation` relative to `root`, percent-encoded like indexed paths; a
    /// directory outside `root` counts as the root.
    pub fn within(root: &Path, invocation: &Path) -> Self {
        invocation
            .strip_prefix(root)
            .ok()
            .map(|relative| {
                Self(
                    crate::store::encode_path(relative)
                        .trim_matches('/')
                        .to_owned(),
                )
            })
            .unwrap_or_default()
    }

    /// Invocation directory relative to the selected root.
    pub(crate) fn member_relative_path(&self) -> Result<PathBuf> {
        crate::store::decode_path(&self.0)
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

/// Whether a definition sits in a test file or inside any `mod tests` scope.
fn is_test_site(hit: &Hit) -> bool {
    is_test_path(&hit.path)
        || hit.container.as_deref().is_some_and(|container| {
            container
                .split("::")
                .any(|segment| matches!(segment, "tests" | "test"))
        })
}

/// Ordering key of one declaration: kind tier, test site, nearness (smaller first).
pub(super) fn rank_key(hit: &Hit, origin: &InvocationDirectory) -> (u8, bool, Reverse<usize>) {
    (
        declaration_rank(&hit.kind),
        is_test_site(hit),
        Reverse(origin.shared_depth(&hit.path)),
    )
}

/// Stable ranking by [`rank_key`], so ties keep path order; callers outside
/// this module use [`super::rank_definitions`].
pub(super) fn rank_declarations(hits: &mut [Hit], origin: &InvocationDirectory) {
    hits.sort_by_key(|hit| rank_key(hit, origin));
}

/// Definitions named by `query.text` (`name` or `Qualifier::name`) under the
/// query's filters (`paths` is `query.path` bound once), in path order.
pub(super) fn declaration_hits(
    store: &Store,
    query: &Query,
    paths: &BoundPathFilter,
) -> Result<LaneHits> {
    let name = QualifiedName::parse(&query.text);
    qualified_hits(store, &name, paths, &query.language, &query.kind)
}

/// Definitions of `name` under path/language/kind filters, in path order.
/// Qualified names keep only the closest match tier before the result cap.
pub(super) fn qualified_hits(
    store: &Store,
    name: &QualifiedName,
    paths: &BoundPathFilter,
    language: &str,
    kind: &str,
) -> Result<LaneHits> {
    qualified_hits_with_limit(store, name, paths, language, kind, MAX_HITS)
}

/// Looks up qualified declarations before applying the result cap, so broad
/// owner-hint matches cannot crowd out a later exact owner.
fn qualified_hits_with_limit(
    store: &Store,
    name: &QualifiedName,
    paths: &BoundPathFilter,
    language: &str,
    kind: &str,
    max_hits: usize,
) -> Result<LaneHits> {
    let mut statement = store.conn.prepare(&format!(
        "SELECT f.path,f.revision,d.start,d.end,d.name,d.kind,d.container,
                'exact_identifier',c.bytes
         FROM definitions d
         JOIN files f ON f.id=d.file_id
         JOIN contents c ON c.revision=f.revision
         WHERE d.name=?1{}
           AND (?2='' OR f.language=?2)
           AND (?3='' OR d.kind=?3)
           AND (?5='' OR instr(coalesce(d.container,'')||'/'||replace(f.path,'-','_'),?5)>0)
         ORDER BY f.path,d.start,d.end,d.id
         LIMIT ?4",
        paths.sql_clause("f.path")
    ))?;
    let mut rows = statement.query(params![
        name.name,
        language,
        kind,
        if name.is_qualified() {
            i64::MAX
        } else {
            max_hits.saturating_add(1).min(i64::MAX as usize) as i64
        },
        name.owner_hint()
    ])?;
    let mut hits = Vec::with_capacity(max_hits.min(128));
    let mut best = None;
    let mut truncated = false;
    let result_cap = max_hits.saturating_add(1);
    while let Some(row) = rows.next()? {
        let hit = hit_row(row)?;
        let Some(tier) = name.matches(&hit.path, hit.container.as_deref()) else {
            continue;
        };
        match best {
            Some(previous) if tier > previous => continue,
            Some(previous) if tier < previous => {
                hits.clear();
                truncated = false;
                best = Some(tier);
            }
            None => best = Some(tier),
            _ => {}
        }
        if hits.len() == result_cap {
            truncated = true;
            if !name.is_qualified() || tier == super::qualified_name::QualifierMatch::Suffix {
                break;
            }
        } else {
            hits.push(hit);
        }
    }
    truncated |= hits.len() > max_hits;
    hits.truncate(max_hits);
    Ok(LaneHits { hits, truncated })
}

#[cfg(test)]
mod tests;
