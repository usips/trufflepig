use anyhow::Result;
use rusqlite::Connection;

pub(super) fn create(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
         INSERT OR IGNORE INTO meta VALUES('generation','0');
         INSERT OR IGNORE INTO meta VALUES('coverage','{\"total_files\":0,\"indexed_files\":0,\"excluded_files\":0,\"parse_failures\":0,\"semantic_files\":0,\"walk_failures\":0,\"truncated_files\":0,\"indexed_bytes\":0}');
         CREATE TABLE IF NOT EXISTS contents(revision TEXT PRIMARY KEY,bytes BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS extraction_cache(revision TEXT NOT NULL,grammar TEXT NOT NULL,version INTEGER NOT NULL,facts TEXT NOT NULL,PRIMARY KEY(revision,grammar,version));
         CREATE TABLE IF NOT EXISTS files(id INTEGER PRIMARY KEY,path TEXT UNIQUE NOT NULL,revision TEXT,language TEXT NOT NULL,status TEXT NOT NULL,bytes INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS definitions(id INTEGER PRIMARY KEY,file_id INTEGER NOT NULL REFERENCES files(id),name TEXT NOT NULL,kind TEXT NOT NULL,start INTEGER NOT NULL,end INTEGER NOT NULL,container TEXT);
         CREATE INDEX IF NOT EXISTS definition_name ON definitions(name);
         CREATE INDEX IF NOT EXISTS definition_file ON definitions(file_id);
         CREATE TABLE IF NOT EXISTS occurrences(id INTEGER PRIMARY KEY,file_id INTEGER NOT NULL REFERENCES files(id),name TEXT NOT NULL,start INTEGER NOT NULL,end INTEGER NOT NULL,role TEXT NOT NULL,target INTEGER REFERENCES definitions(id),candidates TEXT NOT NULL,provenance TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS occurrence_name ON occurrences(name);
         CREATE INDEX IF NOT EXISTS occurrence_target ON occurrences(target);
         CREATE TABLE IF NOT EXISTS relationships(source INTEGER NOT NULL REFERENCES definitions(id),target INTEGER REFERENCES definitions(id),kind TEXT NOT NULL,file_id INTEGER NOT NULL REFERENCES files(id),start INTEGER NOT NULL,end INTEGER NOT NULL,provenance TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS regions(id INTEGER PRIMARY KEY,file_id INTEGER NOT NULL REFERENCES files(id),start INTEGER NOT NULL,end INTEGER NOT NULL,name TEXT NOT NULL,kind TEXT NOT NULL,body TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS region_file ON regions(file_id);
         CREATE VIRTUAL TABLE IF NOT EXISTS documents USING fts5(name,expanded,body,path,tokenize='unicode61');"
    )?;
    Ok(())
}
