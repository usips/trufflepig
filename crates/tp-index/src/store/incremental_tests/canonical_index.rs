//! An id-free rendering of a live index, table by table, so an incremental
//! index can be compared with a full rebuild of the same tree.
use rusqlite::Connection;
use std::collections::BTreeMap;

/// Definition identity without row ids: file path, name, kind, span, container.
const DEFINITION_KEY: &str =
    "{path}|{alias}.name|{alias}.kind|{alias}.start|{alias}.end|coalesce({alias}.container,'-')";

fn definition_key(alias: &str, path: &str) -> String {
    DEFINITION_KEY
        .replace("{alias}", alias)
        .replace("{path}", path)
        .replace('|', "||'|'||")
}

/// Every table's rows as sorted strings; ids, generation, and stat columns are
/// left out because they record when and how a row was written.
pub(super) fn canonical_index(conn: &Connection) -> BTreeMap<&'static str, Vec<String>> {
    let target = definition_key("t", "tf.path");
    let source = definition_key("s", "sf.path");
    let queries: [(&str, String); 10] = [
        (
            "files",
            "SELECT path||'|'||coalesce(revision,'-')||'|'||language||'|'||status||'|'||bytes FROM files".into(),
        ),
        ("contents", "SELECT revision||'|'||hex(bytes) FROM contents".into()),
        (
            "extraction_cache",
            "SELECT revision||'|'||grammar||'|'||version||'|'||facts FROM extraction_cache".into(),
        ),
        (
            "definitions",
            format!("SELECT {} FROM definitions d JOIN files f ON f.id=d.file_id", definition_key("d", "f.path")),
        ),
        (
            "occurrences",
            format!(
                "SELECT f.path||'|'||o.name||'|'||o.start||'|'||o.end||'|'||o.role||'|'||coalesce({target},'-')
                    ||'|'||json_array_length(o.candidates)||'|'||coalesce((SELECT group_concat(k,';') FROM (
                        SELECT {target} AS k FROM json_each(o.candidates) j
                        JOIN definitions t ON t.id=j.value JOIN files tf ON tf.id=t.file_id ORDER BY j.key)),'')
                    ||'|'||o.provenance
                 FROM occurrences o JOIN files f ON f.id=o.file_id
                 LEFT JOIN definitions t ON t.id=o.target LEFT JOIN files tf ON tf.id=t.file_id"
            ),
        ),
        (
            "relationships",
            format!(
                "SELECT f.path||'|'||{source}||'|'||coalesce({target},'-')||'|'||r.kind||'|'||r.start||'|'||r.end||'|'||r.provenance
                 FROM relationships r JOIN files f ON f.id=r.file_id
                 JOIN definitions s ON s.id=r.source JOIN files sf ON sf.id=s.file_id
                 LEFT JOIN definitions t ON t.id=r.target LEFT JOIN files tf ON tf.id=t.file_id"
            ),
        ),
        (
            "regions",
            "SELECT f.path||'|'||r.start||'|'||r.end||'|'||r.name||'|'||r.kind||'|'||r.body
             FROM regions r JOIN files f ON f.id=r.file_id"
                .into(),
        ),
        (
            "documents",
            "SELECT coalesce(f.path,'orphan')||'|'||coalesce(r.start,'-')||'|'||d.name||'|'||d.expanded||'|'||d.body||'|'||d.path
             FROM documents d LEFT JOIN regions r ON r.id=d.rowid LEFT JOIN files f ON f.id=r.file_id"
                .into(),
        ),
        (
            "regions_without_documents",
            "SELECT r.id FROM regions r WHERE r.id NOT IN (SELECT rowid FROM documents)".into(),
        ),
        (
            "meta",
            "SELECT key||'|'||value FROM meta WHERE key IN ('coverage','fingerprint','regions_contract','root')".into(),
        ),
    ];
    queries
        .into_iter()
        .map(|(table, query)| {
            let mut statement = conn
                .prepare(&query)
                .unwrap_or_else(|error| panic!("{table}: {error}"));
            let mut rows: Vec<String> = statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap_or_else(|error| panic!("{table}: {error}"));
            rows.sort();
            (table, rows)
        })
        .collect()
}

/// Panics with the first differing row of the first differing table.
pub(super) fn assert_same_index(step: &str, incremental: &Connection, rebuilt: &Connection) {
    let incremental_rows = canonical_index(incremental);
    let rebuilt_rows = canonical_index(rebuilt);
    for (table, rows) in &rebuilt_rows {
        let other = &incremental_rows[table];
        if other == rows {
            continue;
        }
        let missing: Vec<_> = rows
            .iter()
            .filter(|row| !other.contains(row))
            .take(3)
            .collect();
        let extra: Vec<_> = other
            .iter()
            .filter(|row| !rows.contains(row))
            .take(3)
            .collect();
        panic!(
            "{step}: table {table} differs ({} incremental vs {} rebuilt rows)\nmissing: {missing:#?}\nextra: {extra:#?}",
            other.len(),
            rows.len()
        );
    }
    let dangling: i64 = incremental
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(dangling, 0, "{step}: dangling references");
    incremental
        .execute_batch("INSERT INTO documents(documents) VALUES('integrity-check');")
        .unwrap_or_else(|error| panic!("{step}: FTS integrity: {error}"));
}
