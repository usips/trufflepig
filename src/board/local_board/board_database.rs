//! Secure durable database creation and forward-only schema migrations.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::{BoardError, invalid, sql_error};

const SCHEMA_VERSION: i64 = 1;

#[cfg(test)]
pub(super) fn open(path: &Path) -> Result<(Connection, PathBuf), BoardError> {
    open_with_timeout(path, Duration::from_secs(5))
}

pub(super) fn open_with_timeout(
    path: &Path,
    timeout: Duration,
) -> Result<(Connection, PathBuf), BoardError> {
    let deadline = Instant::now() + timeout.min(Duration::from_secs(5));
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let created_parent = !parent.exists();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(e)
                }
            })
            .map_err(io_error)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(parent).map_err(io_error)?;
    let parent = parent.canonicalize().map_err(io_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = parent.metadata().map_err(io_error)?.permissions().mode();
        if mode & 0o200 == 0 {
            return Err(unavailable("database directory is not writable"));
        }
        if created_parent && mode & 0o077 != 0 {
            return Err(unavailable("new database directory is not private"));
        }
    }
    let name = path
        .file_name()
        .ok_or_else(|| unavailable("database path has no filename"))?;
    let resolved = parent.join(name);
    if resolved
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(unavailable("database path is a symbolic link"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&resolved) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = resolved.metadata().map_err(io_error)?.permissions().mode();
                if mode & 0o077 != 0 {
                    return Err(unavailable("database file permissions must be 0600"));
                }
                if mode & 0o200 == 0 {
                    return Err(unavailable("database file is not writable"));
                }
            }
        }
        Err(error) => return Err(io_error(error)),
    }
    let mut conn = Connection::open(&resolved).map_err(sql_error)?;
    let version: i64 = retry_busy(&conn, deadline, || {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
    })?;
    if version > SCHEMA_VERSION {
        return Err(unavailable(format!(
            "schema version {version} is newer than supported {SCHEMA_VERSION}"
        )));
    }
    let journal: String = retry_busy(&conn, deadline, || {
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
    })?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(unavailable("database cannot enable WAL journal mode"));
    }
    retry_busy(&conn, deadline, || {
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
    })?;
    conn.busy_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(sql_error)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sql_error)?;
    let locked_version: i64 = tx
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql_error)?;
    if locked_version > SCHEMA_VERSION {
        return Err(unavailable("database schema became newer than supported"));
    }
    if locked_version < SCHEMA_VERSION {
        tx.execute_batch(SCHEMA).map_err(sql_error)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(sql_error)?;
    }
    tx.execute("INSERT INTO board_meta(key,value) VALUES('resolved_path',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [resolved.to_string_lossy().as_ref()]).map_err(sql_error)?;
    tx.commit().map_err(sql_error)?;
    conn.busy_timeout(timeout.min(Duration::from_secs(5)))
        .map_err(sql_error)?;
    Ok((conn, resolved))
}

fn retry_busy<T>(
    conn: &Connection,
    deadline: Instant,
    mut operation: impl FnMut() -> rusqlite::Result<T>,
) -> Result<T, BoardError> {
    loop {
        conn.busy_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(sql_error)?;
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                let busy = matches!(&error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked));
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !busy || remaining.is_zero() {
                    return Err(sql_error(error));
                }
                // Journal-mode lock promotion can fail without invoking SQLite's busy handler.
                std::thread::sleep(remaining.min(Duration::from_millis(1)));
            }
        }
    }
}

fn io_error(error: std::io::Error) -> BoardError {
    unavailable(error.to_string())
}

fn unavailable(message: impl Into<String>) -> BoardError {
    invalid("board_unavailable", message)
}

const SCHEMA: &str = r#"
CREATE TABLE board_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE actors(
 id INTEGER PRIMARY KEY, user TEXT NOT NULL, host TEXT NOT NULL,
 harness TEXT NOT NULL, session TEXT NOT NULL, UNIQUE(user,host,harness,session)
);
CREATE TABLE agent_sessions(
 actor_id INTEGER PRIMARY KEY REFERENCES actors(id), model TEXT, effort TEXT,
 cursor_seq INTEGER, bound_plan INTEGER, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL
);
CREATE TABLE repos(repo_key TEXT PRIMARY KEY, origin_label TEXT);
CREATE TABLE repo_paths(
 repo_key TEXT NOT NULL REFERENCES repos(repo_key), host TEXT NOT NULL,
 common_dir TEXT NOT NULL, scan_error TEXT, tips_digest TEXT,
 PRIMARY KEY(repo_key,host,common_dir)
);
CREATE TABLE texts(hash TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE plans(
 id INTEGER PRIMARY KEY, title TEXT NOT NULL, owner_user TEXT NOT NULL,
 steward TEXT, head_revision INTEGER NOT NULL CHECK(head_revision>0),
 next_task INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL
);
CREATE TABLE plan_repos(
 plan_id INTEGER NOT NULL REFERENCES plans(id), repo_key TEXT NOT NULL REFERENCES repos(repo_key),
 PRIMARY KEY(plan_id,repo_key)
);
CREATE TABLE entries(
 id INTEGER PRIMARY KEY, plan_id INTEGER REFERENCES plans(id), kind TEXT NOT NULL,
 body TEXT NOT NULL, to_whom TEXT, supersedes INTEGER REFERENCES entries(id),
 actor_id INTEGER NOT NULL REFERENCES actors(id), model TEXT, effort TEXT,
 repo_key TEXT REFERENCES repos(repo_key), state TEXT, dedupe_key TEXT,
 seq INTEGER NOT NULL, created_at INTEGER NOT NULL
);
CREATE INDEX entries_plan_sequence ON entries(plan_id,seq);
CREATE INDEX entries_dedupe ON entries(actor_id,dedupe_key,created_at);
CREATE TABLE entry_refs(
 entry_id INTEGER NOT NULL REFERENCES entries(id), target TEXT NOT NULL,
 PRIMARY KEY(entry_id,target)
);
CREATE INDEX entry_refs_target ON entry_refs(target);
CREATE TABLE revisions(
 plan_id INTEGER NOT NULL REFERENCES plans(id), number INTEGER NOT NULL CHECK(number>0),
 text_hash TEXT NOT NULL REFERENCES texts(hash), source TEXT NOT NULL CHECK(source IN ('create','accept','direct')),
 entry_id INTEGER REFERENCES entries(id), actor_id INTEGER NOT NULL REFERENCES actors(id),
 seq INTEGER NOT NULL, PRIMARY KEY(plan_id,number)
);
CREATE TABLE proposals(
 entry_id INTEGER PRIMARY KEY REFERENCES entries(id), plan_id INTEGER NOT NULL REFERENCES plans(id),
 base_revision INTEGER NOT NULL, text_hash TEXT NOT NULL REFERENCES texts(hash),
 state TEXT NOT NULL CHECK(state IN ('open','accepted','rejected')),
 decision_entry INTEGER REFERENCES entries(id), result_revision INTEGER,
 FOREIGN KEY(plan_id,base_revision) REFERENCES revisions(plan_id,number)
);
CREATE TABLE tasks(
 plan_id INTEGER NOT NULL REFERENCES plans(id), ordinal INTEGER NOT NULL CHECK(ordinal>0),
 title TEXT NOT NULL, column_name TEXT NOT NULL CHECK(column_name IN ('todo','doing','review','done','blocked')),
 assignee TEXT, section TEXT, seq INTEGER NOT NULL, PRIMARY KEY(plan_id,ordinal)
);
CREATE TABLE claims(
 id INTEGER PRIMARY KEY, plan_id INTEGER NOT NULL, task_ordinal INTEGER NOT NULL,
 actor_id INTEGER NOT NULL REFERENCES actors(id), entry_id INTEGER NOT NULL REFERENCES entries(id),
 scope TEXT NOT NULL, claimed_at INTEGER NOT NULL, last_active INTEGER NOT NULL,
 ended_at INTEGER, end_reason TEXT CHECK(end_reason IN ('released','taken_over','reassigned')),
 FOREIGN KEY(plan_id,task_ordinal) REFERENCES tasks(plan_id,ordinal)
);
CREATE UNIQUE INDEX claims_one_active ON claims(plan_id,task_ordinal) WHERE ended_at IS NULL;
CREATE INDEX claims_actor_active ON claims(actor_id,ended_at);
CREATE TABLE commits(
 repo_key TEXT NOT NULL REFERENCES repos(repo_key), oid TEXT NOT NULL, subject TEXT NOT NULL,
 committed_at INTEGER NOT NULL, author TEXT NOT NULL, coauthors TEXT NOT NULL,
 files INTEGER NOT NULL, file_stats TEXT NOT NULL, insertions INTEGER NOT NULL, deletions INTEGER NOT NULL,
 PRIMARY KEY(repo_key,oid)
);
CREATE TABLE commit_plans(
 repo_key TEXT NOT NULL, oid TEXT NOT NULL, plan_id INTEGER NOT NULL REFERENCES plans(id),
 task_ordinal INTEGER, entry_id INTEGER NOT NULL REFERENCES entries(id),
 PRIMARY KEY(repo_key,oid,plan_id), FOREIGN KEY(repo_key,oid) REFERENCES commits(repo_key,oid),
 FOREIGN KEY(plan_id,task_ordinal) REFERENCES tasks(plan_id,ordinal)
);
CREATE TABLE board_feedback(
 entry_id INTEGER PRIMARY KEY REFERENCES entries(id), feedback_kind TEXT NOT NULL,
 version TEXT NOT NULL, build_id TEXT, cwd TEXT NOT NULL, steer_mode TEXT,
 recent_calls_json TEXT NOT NULL, import_key TEXT UNIQUE
);
CREATE TABLE feedback_imports(
 import_key TEXT PRIMARY KEY, entry_id INTEGER NOT NULL REFERENCES entries(id)
);
CREATE TABLE events(
 seq INTEGER PRIMARY KEY, plan_id INTEGER REFERENCES plans(id), kind TEXT NOT NULL,
 subject TEXT NOT NULL, to_whom TEXT, actor_id INTEGER NOT NULL REFERENCES actors(id),
 summary TEXT NOT NULL, created_at INTEGER NOT NULL
);
CREATE INDEX events_plan_sequence ON events(plan_id,seq);
CREATE INDEX events_recipient_sequence ON events(to_whom,seq);
CREATE TABLE operation_dedupes(
 dedupe_key TEXT PRIMARY KEY, reply_json TEXT NOT NULL, created_at INTEGER NOT NULL
);
"#;
