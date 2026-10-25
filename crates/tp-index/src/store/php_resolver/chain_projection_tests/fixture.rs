use crate::store::Store;
use rusqlite::{OptionalExtension, params};

pub(super) const XF_ALIAS_SOURCE: &str = r#"<?php
class XF
{
    public static function getAliasableNamespaces(): array
    {
        return ['Controller'];
    }

    public static function getUnaliasableNamespaces(): array
    {
        return [];
    }
}
"#;

pub(super) struct IndexedFixture {
    _root: tempfile::TempDir,
    _cache: tempfile::TempDir,
    pub(super) store: Store,
}

impl IndexedFixture {
    pub(super) fn new(files: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        for &(path, source) in files {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source.as_bytes()).unwrap();
        }
        let mut store = Store::open(root.path(), cache.path()).unwrap();
        store.index().unwrap();
        Self {
            _root: root,
            _cache: cache,
            store,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SqlRelationship {
    pub(super) source: i64,
    pub(super) target: Option<i64>,
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) provenance: String,
}

pub(super) fn relationships(fixture: &IndexedFixture, kind: &str) -> Vec<SqlRelationship> {
    let mut statement = fixture
        .store
        .conn
        .prepare(
            "SELECT source,target,start,end,provenance FROM relationships WHERE kind=?1 ORDER BY source,target,start,end",
        )
        .unwrap();
    statement
        .query_map([kind], |row| {
            Ok(SqlRelationship {
                source: row.get(0)?,
                target: row.get(1)?,
                start: row.get(2)?,
                end: row.get(3)?,
                provenance: row.get(4)?,
            })
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

pub(super) fn class_id(fixture: &IndexedFixture, fqcn: &str) -> i64 {
    let (namespace, name) = fqcn.rsplit_once('\\').unwrap_or(("", fqcn));
    fixture
        .store
        .conn
        .query_row(
            "SELECT d.id FROM definitions d JOIN files f ON f.id=d.file_id WHERE d.kind IN ('class','interface','trait','enum') AND d.name=?1 AND coalesce(d.container,'')=?2 ORDER BY d.id LIMIT 1",
            params![name, namespace],
            |row| row.get(0),
        )
        .unwrap()
}

pub(super) fn definition_id(
    fixture: &IndexedFixture,
    path: &str,
    kind: &str,
    name: &str,
) -> Option<i64> {
    fixture
        .store
        .conn
        .query_row(
            "SELECT d.id FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.path=?1 AND d.kind=?2 AND d.name=?3 ORDER BY d.id LIMIT 1",
            params![path, kind, name],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

pub(super) fn method_id(fixture: &IndexedFixture, class: &str, name: &str) -> Option<i64> {
    fixture
        .store
        .conn
        .query_row(
            "SELECT id FROM definitions WHERE kind='method' AND name=?1 AND container=?2 ORDER BY id LIMIT 1",
            params![name, class],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SqlOccurrence {
    pub(super) name: String,
    pub(super) target: Option<i64>,
    pub(super) candidates: Vec<i64>,
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) provenance: String,
}

pub(super) fn occurrences(
    fixture: &IndexedFixture,
    path: &str,
    role: &str,
    provenance: &str,
) -> Vec<SqlOccurrence> {
    let mut statement = fixture
        .store
        .conn
        .prepare(
            "SELECT o.name,o.target,o.candidates,o.start,o.end,o.provenance FROM occurrences o JOIN files f ON f.id=o.file_id WHERE f.path=?1 AND o.role=?2 AND o.provenance LIKE ?3 ORDER BY o.start,o.end",
        )
        .unwrap();
    statement
        .query_map(params![path, role, format!("{provenance}%")], |row| {
            let encoded: String = row.get(2)?;
            Ok(SqlOccurrence {
                name: row.get(0)?,
                target: row.get(1)?,
                candidates: serde_json::from_str(&encoded).unwrap(),
                start: row.get(3)?,
                end: row.get(4)?,
                provenance: row.get(5)?,
            })
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

pub(super) fn placeholder_occurrences(fixture: &IndexedFixture, path: &str) -> Vec<SqlOccurrence> {
    let mut statement = fixture
        .store
        .conn
        .prepare(
            "SELECT o.name,o.target,o.candidates,o.start,o.end,o.provenance FROM occurrences o JOIN files f ON f.id=o.file_id WHERE f.path=?1 AND o.role='type' AND instr(o.name,'XFCP_')>0 ORDER BY o.start,o.end",
        )
        .unwrap();
    statement
        .query_map([path], |row| {
            let encoded: String = row.get(2)?;
            Ok(SqlOccurrence {
                name: row.get(0)?,
                target: row.get(1)?,
                candidates: serde_json::from_str(&encoded).unwrap(),
                start: row.get(3)?,
                end: row.get(4)?,
                provenance: row.get(5)?,
            })
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

pub(super) fn definition_count(fixture: &IndexedFixture, path: &str, kind: &str) -> i64 {
    fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.path=?1 AND d.kind=?2",
            params![path, kind],
            |row| row.get(0),
        )
        .unwrap()
}

pub(super) fn no_internal_markers_or_synthetic_definitions(fixture: &IndexedFixture) {
    let markers: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM relationships WHERE kind='__trufflepig_php_marker_v1' OR provenance LIKE 'php_fact_v1:%') + (SELECT count(*) FROM occurrences WHERE provenance='__trufflepig_php_marker_v1' OR provenance LIKE 'php_fact_v1:%')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        markers, 0,
        "internal PHP markers must be removed after projection"
    );

    let synthetic: Vec<(String, String, String)> = fixture
        .store
        .conn
        .prepare(
            "SELECT f.path,d.kind,d.name FROM definitions d JOIN files f ON f.id=d.file_id WHERE d.kind LIKE '%generated%' OR (d.kind IN ('class','interface','trait','enum') AND d.name GLOB 'XFCP_*') ORDER BY f.path,d.name",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        synthetic.len(),
        0,
        "projection must not invent PHP definitions: {synthetic:?}"
    );
}

pub(super) fn byte_span(source: &str, text: &str) -> (i64, i64) {
    let start = source.find(text).unwrap() as i64;
    (start, start + text.len() as i64)
}
