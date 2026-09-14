use super::Coverage;
use anyhow::Result;
use rusqlite::{Connection, params};
use std::path::Path;

pub(super) fn publish(
    conn: &mut Connection,
    staged: &Path,
    coverage: &Coverage,
    fingerprint: &str,
) -> Result<()> {
    conn.execute(
        "ATTACH DATABASE ?1 AS staged",
        [staged
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 cache directory"))?],
    )?;
    let result: Result<()> = (|| {
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "DELETE FROM relationships; DELETE FROM occurrences; DELETE FROM definitions;
             DELETE FROM documents; DELETE FROM regions; DELETE FROM files;
             INSERT OR IGNORE INTO contents SELECT * FROM staged.contents;
             INSERT INTO files SELECT * FROM staged.files;
             INSERT INTO definitions SELECT * FROM staged.definitions;
             INSERT INTO occurrences SELECT * FROM staged.occurrences;
             INSERT INTO relationships SELECT * FROM staged.relationships;
             INSERT INTO regions SELECT * FROM staged.regions;
             INSERT INTO documents(rowid,name,expanded,body,path) SELECT rowid,name,expanded,body,path FROM staged.documents;
             DELETE FROM extraction_cache;
             INSERT INTO extraction_cache SELECT * FROM staged.extraction_cache;
             DELETE FROM contents WHERE revision NOT IN (SELECT revision FROM files WHERE revision IS NOT NULL);
             UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='generation';"
        )?;
        transaction.execute(
            "UPDATE meta SET value=?1 WHERE key='coverage'",
            [serde_json::to_string(coverage)?],
        )?;
        transaction.execute("INSERT INTO meta(key,value) VALUES('fingerprint',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![fingerprint])?;
        transaction.commit()?;
        Ok(())
    })();
    let detach = conn.execute_batch("DETACH DATABASE staged");
    result?;
    detach?;
    Ok(())
}
