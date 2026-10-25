//! Bounded workspace snapshots retain immutable entries independently of member caches.
mod coverage_lines;
mod index_progress_source;
use super::member_root::{GIT_PROBE_TIMEOUT, MemberRoot, linked_root_of_member};
use crate::{
    results::{self, HitDetail, ResultEntry},
    store::Store,
};
use anyhow::{Result, bail, ensure};
use coverage_lines::{compact_coverage, mark_worktree_differs, member_coverage_summary};
pub(super) use index_progress_source::IndexProgressSource;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    thread,
    time::Duration,
};
use tp_core::coded_error::CodedContext;
use tp_core::workspace_config::WorkspaceConfig;
use tp_core::{
    identity::{ResultCursor, ResultHandle},
    output::{OutputBudget, OutputFormat},
    path_codec::{decode_path, encode_path},
    query_deadline::QueryDeadline,
};

const WORKSPACE_SQL_BUSY_SLICE: Duration = Duration::from_millis(25);
const WORKSPACE_BUSY_RETRY_PAUSE: Duration = Duration::from_millis(3);
const WORKSPACE_PROGRESS_STEPS: i32 = 4_096;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MemberSnapshot {
    pub name: String,
    pub root: String,
    pub cache: String,
    pub device: u64,
    pub inode: u64,
    pub index_identity: String,
    pub generation: i64,
    pub coverage: Value,
    /// Worktree label when the owning root is a linked worktree of the member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// The parent member index that answered for a warming worktree; follow-ups
    /// reopen that same view even after the worktree publishes its own index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_index: Option<ParentIndexRecord>,
}

/// Root and cache of the parent member index a worktree result was served from.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ParentIndexRecord {
    pub root: String,
    pub cache: String,
}
impl MemberSnapshot {
    pub fn capture(
        member: &MemberRoot,
        store: &Store,
        cache: &Path,
        generation: i64,
    ) -> Result<Self> {
        member.verify_identity()?;
        let identity = member.identity.as_ref().with_coded_context(|| {
            coded!(MemberUnavailable, "original root identity is missing")
        })?;
        let deadline = store.query_deadline();
        let check_deadline = || -> Result<()> {
            if deadline.is_some_and(QueryDeadline::expired) {
                bail!(coded!(TimedOut, "query deadline expired"));
            }
            Ok(())
        };
        check_deadline()?;
        if let Err(error) = std::fs::create_dir_all(cache) {
            let error = crate::store::index_cache_unavailable(cache, error.into());
            check_deadline()?;
            return Err(error);
        }
        check_deadline()?;
        let canonical_cache = match cache.canonicalize() {
            Ok(cache) => cache,
            Err(error) => {
                let error = crate::store::index_cache_unavailable(cache, error.into());
                check_deadline()?;
                return Err(error);
            }
        };
        check_deadline()?;
        let index_identity = store
            .conn
            .query_row("SELECT value FROM meta WHERE key='index_epoch'", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_else(|| "unpublished".to_owned());
        Ok(Self {
            name: member.name().to_owned(),
            root: encode_path(&member.root),
            cache: encode_path(&canonical_cache),
            device: identity.device,
            inode: identity.inode,
            index_identity,
            generation,
            coverage: serde_json::to_value(store.coverage()?)?,
            worktree: member.worktree.clone(),
            parent_index: (store.index_root() != store.root).then(|| ParentIndexRecord {
                root: encode_path(store.index_root()),
                cache: encode_path(store.index_cache()),
            }),
        })
    }
    /// The owning member with the recorded root; a worktree owner must still be
    /// a linked worktree of its configured member.
    pub fn member_root(&self, config: &WorkspaceConfig) -> Result<MemberRoot> {
        let member = config
            .members
            .iter()
            .find(|member| member.name == self.name)
            .with_coded_context(|| {
                coded!(MemberUnavailable, "result owner was removed from workspace")
            })?;
        if self.worktree.is_none() {
            ensure!(
                encode_path(&member.root) == self.root,
                coded!(MemberUnavailable, "result owner path changed")
            );
            return Ok(MemberRoot::configured(member));
        }
        let root = decode_path(&self.root)?;
        let root = linked_root_of_member(member, &root, GIT_PROBE_TIMEOUT)
            .with_coded_context(|| coded!(MemberUnavailable, "result owner is missing"))?;
        let mut owner = MemberRoot::linked(member, root)?;
        owner.is_home = true;
        Ok(owner)
    }
    /// Reopens the owner's index query-only; it must be the same index database.
    pub fn open(&self, config: &WorkspaceConfig, deadline: &QueryDeadline) -> Result<Store> {
        let member = self.member_root(config)?;
        let metadata = member
            .root
            .metadata()
            .with_coded_context(|| coded!(MemberUnavailable, "result owner is missing"))?;
        ensure!(
            metadata.dev() == self.device
                && metadata.ino() == self.inode
                && member.root.canonicalize()? == member.root,
            coded!(MemberUnavailable, "result owner was replaced")
        );
        let cache = decode_path(&self.cache)?;
        if self.generation == 0 {
            // A path read can own immutable bytes without any graph publication.
            // Keep this owner unpublished even if the worktree indexes later.
            return Ok(
                Store::unpublished(&member.root, &cache)?.with_query_deadline(deadline.clone())
            );
        }
        let store = if let Some(parent) = &self.parent_index {
            ensure!(
                encode_path(&member.member.root) == parent.root,
                coded!(MemberUnavailable, "parent index owner changed")
            );
            let parent_cache = decode_path(&parent.cache)?;
            ensure!(
                parent_cache.join("index.sqlite3").is_file(),
                coded!(MemberUnavailable, "owning index was removed")
            );
            Store::open_read(&member.member.root, &parent_cache, deadline.clone())?
                .reading_from(&member.root, &cache)?
        } else {
            ensure!(
                cache.join("index.sqlite3").is_file(),
                coded!(MemberUnavailable, "owning index was removed")
            );
            Store::open_read(&member.root, &cache, deadline.clone())?
        };
        let current: Option<String> = store
            .conn
            .query_row("SELECT value FROM meta WHERE key='index_epoch'", [], |r| {
                r.get(0)
            })
            .optional()?;
        ensure!(
            current.as_ref() == Some(&self.index_identity),
            coded!(MemberUnavailable, "owning index was replaced")
        );
        Ok(store)
    }
    pub fn metadata(&self, workspace: &str) -> Value {
        let mut value = json!({"workspace":workspace,"member":self.name,"repository":self.root});
        if let Some(label) = &self.worktree {
            value["worktree"] = label.clone().into();
        }
        if self.parent_index.is_some() {
            value["served_from"] = format!("{} index", self.name).into();
        }
        value
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OwnedEntry {
    pub owner: usize,
    pub member_rank: usize,
    pub entry: ResultEntry,
    /// The hit's file may differ between the worktree and the parent index that answered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worktree_differs: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceSet {
    pub workspace: String,
    pub home: Option<String>,
    pub owners: Vec<MemberSnapshot>,
    pub coverage: Vec<Value>,
    pub hits: Vec<OwnedEntry>,
    pub truncated: bool,
    /// How an unselected query was scoped: `home (ws:all adds N members)` or
    /// `all (no home hits)`. Absent when the query named its scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}
pub struct WorkspaceResults {
    pub(super) conn: Connection,
    deadline: QueryDeadline,
}
impl WorkspaceResults {
    /// Creates the private cache and its schema; runs once per coordinator start
    /// (or local command), never per request.
    pub fn create(cache: &Path, deadline: &QueryDeadline) -> Result<()> {
        Self::create_schema(cache, deadline).map_err(|error| {
            crate::store::index_cache_unavailable(cache, classify_db_error(deadline, error))
        })
    }

    fn create_schema(cache: &Path, deadline: &QueryDeadline) -> Result<()> {
        check_deadline(deadline)?;
        std::fs::create_dir_all(cache)?;
        std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
        let conn = Connection::open(cache.join("workspace.sqlite3"))
            .map_err(|error| classify_db_error(deadline, error.into()))?;
        install_deadline(&conn, deadline)?;
        execute_workspace_sql(&conn, deadline, || {
            conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS workspace_results(id TEXT PRIMARY KEY, expires INTEGER NOT NULL, payload TEXT NOT NULL)")
        })?;
        check_deadline(deadline)?;
        Ok(())
    }
    /// Opens the store [`Self::create`] made, running no DDL.
    pub fn open(cache: &Path, deadline: &QueryDeadline) -> Result<Self> {
        check_deadline(deadline)?;
        let conn = Connection::open_with_flags(
            cache.join("workspace.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| {
            classify_db_error(
                deadline,
                anyhow::Error::new(error).with_coded_context(|| {
                    coded!(WorkspaceUnavailable, "workspace result store is missing")
                }),
            )
        })?;
        install_deadline(&conn, deadline)?;
        execute_workspace_sql(&conn, deadline, || {
            conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA journal_size_limit=4194304;")
        })?;
        check_deadline(deadline)?;
        Ok(Self {
            conn,
            deadline: deadline.clone(),
        })
    }
    /// The absolute request budget used for all operations on this request's store handle.
    pub fn deadline(&self) -> &QueryDeadline {
        &self.deadline
    }
    pub fn save(&self, mut set: WorkspaceSet) -> Result<String> {
        check_deadline(&self.deadline)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        set.truncated |= set.hits.len() > results::MAX_HITS;
        set.hits.truncate(results::MAX_HITS);
        for (index, hit) in set.hits.iter_mut().enumerate() {
            *hit.entry.handle_mut() = format!("{id}:{}", index + 1);
        }
        refresh_retained_counts(&mut set);
        let mut payload = serde_json::to_string(&set)?;
        while payload.len() > results::MAX_BYTES && !set.hits.is_empty() {
            let keep = (set.hits.len() * results::MAX_BYTES / payload.len()).saturating_sub(1);
            set.hits.truncate(keep);
            set.truncated = true;
            refresh_retained_counts(&mut set);
            payload = serde_json::to_string(&set)?;
        }
        ensure!(
            payload.len() <= results::MAX_BYTES,
            coded!(
                ResultCacheUnavailable,
                "workspace metadata exceeds capacity"
            )
        );
        check_deadline(&self.deadline)?;
        let tx = execute_workspace_sql(&self.conn, &self.deadline, || {
            rusqlite::Transaction::new_unchecked(
                &self.conn,
                rusqlite::TransactionBehavior::Immediate,
            )
        })?;
        execute_workspace_sql(&self.conn, &self.deadline, || {
            tx.execute(
                "DELETE FROM workspace_results WHERE expires<=?1",
                [results::now()],
            )
        })?;
        check_deadline(&self.deadline)?;
        execute_workspace_sql(&self.conn, &self.deadline, || {
            tx.execute(
                "INSERT INTO workspace_results VALUES(?1,?2,?3)",
                params![id, results::now() + 600, payload],
            )
        })?;
        loop {
            check_deadline(&self.deadline)?;
            let (count, bytes): (usize, usize) = execute_workspace_sql(
                &self.conn,
                &self.deadline,
                || {
                    tx.query_row(
                        "SELECT count(*),coalesce(sum(length(CAST(payload AS BLOB))),0) FROM workspace_results",
                        [],
                        |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize)),
                    )
                },
            )?;
            if count <= 50 && bytes <= results::MAX_BYTES {
                break;
            }
            execute_workspace_sql(&self.conn, &self.deadline, || {
                tx.execute("DELETE FROM workspace_results WHERE id=(SELECT id FROM workspace_results WHERE id<>?1 ORDER BY expires,id LIMIT 1)", [&id])
            })?;
        }
        check_deadline(&self.deadline)?;
        // Retry COMMIT while RAII rollback remains available; after success, Drop sees autocommit.
        execute_workspace_sql(&self.conn, &self.deadline, || tx.execute_batch("COMMIT"))?;
        drop(tx);
        check_deadline(&self.deadline)?;
        Ok(id)
    }
    pub fn load(&self, id: &str) -> Result<WorkspaceSet> {
        check_deadline(&self.deadline)?;
        uuid::Uuid::parse_str(id)
            .with_coded_context(|| coded!(InvalidHandle, "invalid set identifier"))?;
        let payload: Option<String> = execute_workspace_sql(&self.conn, &self.deadline, || {
            self.conn
                .query_row(
                    "SELECT payload FROM workspace_results WHERE id=?1 AND expires>?2",
                    params![id, results::now()],
                    |r| r.get(0),
                )
                .optional()
        })?;
        check_deadline(&self.deadline)?;
        let payload = payload.with_coded_context(|| {
            coded!(
                ExpiredResult,
                "workspace result expired or was evicted; search again"
            )
        })?;
        ensure!(
            payload.len() <= results::MAX_BYTES,
            coded!(ResultUnavailable, "oversized workspace result")
        );
        let set: WorkspaceSet = serde_json::from_str(&payload)
            .with_coded_context(|| coded!(ResultUnavailable, "invalid workspace result"))?;
        ensure!(
            set.owners.len() <= 32
                && set.hits.len() <= results::MAX_HITS
                && set.hits.iter().all(|hit| hit.owner < set.owners.len()),
            coded!(ResultUnavailable, "invalid workspace ownership")
        );
        check_deadline(&self.deadline)?;
        Ok(set)
    }
    pub fn entry(&self, handle: &str) -> Result<(WorkspaceSet, usize)> {
        let handle: ResultHandle = handle.parse()?;
        let set = self.load(&handle.set.simple().to_string())?;
        ensure!(
            handle.ordinal <= set.hits.len(),
            coded!(InvalidHandle, "ordinal outside result set")
        );
        Ok((set, handle.ordinal - 1))
    }
    pub fn more(&self, cursor: &str, limit: usize, budget: &OutputBudget) -> Result<String> {
        let cursor: ResultCursor = cursor.parse()?;
        self.page(
            &cursor.set.simple().to_string(),
            cursor.offset,
            limit,
            budget,
        )
    }
    pub fn page(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
        budget: &OutputBudget,
    ) -> Result<String> {
        let mut set = self.load(id)?;
        check_deadline(&self.deadline)?;
        refresh_index_progress(&mut set, &self.deadline)?;
        ensure!(
            offset <= set.hits.len(),
            coded!(InvalidCursor, "offset outside result set")
        );
        let available = set.hits.len() - offset;
        let max_count = available.min(limit).min(budget.limit);
        let mut coverage_line = match &set.scope {
            Some(scope) => format!("{}; scope {scope}", member_coverage_summary(&set.coverage)),
            None => member_coverage_summary(&set.coverage),
        };
        if set.hits.is_empty() {
            coverage_line.push_str(&filter_diagnoses(&set.owners));
        }
        let render_detail = |count: usize, detail: HitDetail| -> Result<String> {
            check_deadline(&self.deadline)?;
            match budget.format {
                OutputFormat::Json => budget.encode(&page_value(&set, id, offset, count, detail)?),
                OutputFormat::Lines => {
                    let cursor =
                        results::result_lines::next_cursor(id, offset, count, set.hits.len());
                    let text = results::result_lines::page_text(
                        set.hits[offset..offset + count].iter().map(|owned| {
                            (Some(set.owners[owned.owner].name.as_str()), &owned.entry)
                        }),
                        detail,
                        &coverage_line,
                        cursor.as_deref().map(|cursor| {
                            results::result_lines::more_command(cursor, offset, count, limit)
                        }),
                        set.truncated,
                    );
                    Ok(mark_worktree_differs(
                        text,
                        &set.hits[offset..offset + count],
                    ))
                }
            }
        };
        if let Some(text) = results::snippet_page(max_count, budget, render_detail)? {
            return Ok(text);
        }
        let render = |count: usize, names: bool| {
            let detail = HitDetail {
                names,
                snippets: false,
            };
            render_detail(count, detail)
        };
        for names in [false, true] {
            let low = results::largest_fitting(max_count, |count| {
                Ok(budget.fits(&render(count, names)?))
            })?;
            if low > 0 {
                let named = render(low, true)?;
                if budget.fits(&named) {
                    return Ok(named);
                }
                return render(low, names);
            }
        }
        if available == 0 {
            let text = render(0, false)?;
            if budget.fits(&text) {
                return Ok(text);
            }
        }
        bail!(coded!(
            BudgetTooSmall,
            "workspace coverage and one result do not fit"
        ));
    }
}

fn check_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        bail!(coded!(TimedOut, "query deadline expired"));
    }
    Ok(())
}

fn refresh_index_progress(set: &mut WorkspaceSet, deadline: &QueryDeadline) -> Result<()> {
    check_deadline(deadline)?;
    for owner in &mut set.owners {
        if let Some(coverage) = owner.coverage.as_object_mut() {
            coverage.remove("index_progress");
            coverage.remove("index_progress_source");
        }
    }
    for coverage in &mut set.coverage {
        let source = coverage
            .get("index_progress_source")
            .cloned()
            .and_then(|value| serde_json::from_value::<IndexProgressSource>(value).ok());
        if let Some(values) = coverage.as_object_mut() {
            values.remove("index_progress");
            values.remove("index_progress_source");
        }
        let Some(source) = source else {
            continue;
        };
        check_deadline(deadline)?;
        let progress = source.read();
        check_deadline(deadline)?;
        if let (Some(progress), Some(values)) = (progress, coverage.as_object_mut()) {
            values.insert("index_progress".into(), serde_json::to_value(progress)?);
        }
    }
    check_deadline(deadline)
}

fn install_deadline(connection: &Connection, deadline: &QueryDeadline) -> Result<()> {
    check_deadline(deadline)?;
    connection.busy_timeout(deadline.cap(WORKSPACE_SQL_BUSY_SLICE))?;
    let progress_deadline = deadline.clone();
    connection.progress_handler(
        WORKSPACE_PROGRESS_STEPS,
        Some(move || progress_deadline.expired()),
    )?;
    check_deadline(deadline)
}

fn classify_db_error(deadline: &QueryDeadline, error: anyhow::Error) -> anyhow::Error {
    let error = deadline.classify(error);
    if deadline.expired() && !tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error) {
        error.with_coded_context(|| coded!(TimedOut, "query deadline expired"))
    } else {
        error
    }
}

fn execute_workspace_sql<T>(
    connection: &Connection,
    deadline: &QueryDeadline,
    mut operation: impl FnMut() -> rusqlite::Result<T>,
) -> Result<T> {
    loop {
        check_deadline(deadline)?;
        connection
            .busy_timeout(deadline.cap(WORKSPACE_SQL_BUSY_SLICE))
            .map_err(|error| classify_db_error(deadline, error.into()))?;
        match operation() {
            Ok(value) => {
                check_deadline(deadline)?;
                return Ok(value);
            }
            Err(error) if is_sqlite_busy(&error) => {
                check_deadline(deadline)?;
                let pause = deadline.cap(WORKSPACE_BUSY_RETRY_PAUSE);
                if pause.is_zero() {
                    check_deadline(deadline)?;
                    bail!(coded!(TimedOut, "query deadline expired"));
                }
                thread::sleep(pause);
            }
            Err(error) => return Err(classify_db_error(deadline, error.into())),
        }
    }
}

fn is_sqlite_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DatabaseBusy
    )
}

fn page_value(
    set: &WorkspaceSet,
    id: &str,
    offset: usize,
    count: usize,
    detail: HitDetail,
) -> Result<Value> {
    let mut hits = Vec::with_capacity(count);
    for owned in &set.hits[offset..offset + count] {
        let owner = &set.owners[owned.owner];
        let root = decode_path(&owner.root)?;
        let mut entry = results::compact_entry(&owned.entry, &root, Some(&owner.name), detail)?;
        if entry.is_object() {
            entry["member"] = owner.name.clone().into();
            if !matches!(&owned.entry, ResultEntry::LiveSource(_)) {
                entry["member_rank"] = owned.member_rank.into();
            }
            if owned.worktree_differs {
                entry["differs"] = true.into();
            }
        }
        hits.push(entry);
    }
    let next = (offset + count < set.hits.len()).then(|| format!("{id}@{}", offset + count));
    Ok(json!({
        "workspace": set.workspace,
        "home": set.home,
        "scope": set.scope,
        "coverage": compact_coverage(&set.coverage),
        "hits": hits,
        "next": next,
        "truncated": set.truncated,
        "tokenizer": "o200k_base"
    }))
}

/// `; MEMBER[, MEMBER]: DIAGNOSIS` for each distinct zero-hit filter explanation.
fn filter_diagnoses(owners: &[MemberSnapshot]) -> String {
    let mut grouped: Vec<(&str, Vec<&str>)> = Vec::with_capacity(owners.len());
    for owner in owners {
        let Some(diagnosis) = owner.coverage["filter_diagnosis"].as_str() else {
            continue;
        };
        match grouped.iter_mut().find(|(known, _)| *known == diagnosis) {
            Some((_, members)) => members.push(&owner.name),
            None => grouped.push((diagnosis, vec![&owner.name])),
        }
    }
    let mut text = String::new();
    for (diagnosis, members) in grouped {
        text.push_str(&format!("; {}: {diagnosis}", members.join(", ")));
    }
    text
}

fn refresh_retained_counts(set: &mut WorkspaceSet) {
    let mut counts = vec![0; set.owners.len()];
    for hit in &set.hits {
        counts[hit.owner] += 1;
    }
    for (owner, count) in set.owners.iter().zip(counts) {
        if let Some(coverage) = set.coverage.iter_mut().find(|v| v["member"] == owner.name) {
            if coverage["retained"]
                .as_u64()
                .is_some_and(|old| old > count as u64)
            {
                coverage["truncated"] = true.into();
            }
            coverage["retained"] = count.into();
        }
    }
}

#[cfg(test)]
mod deadline_tests;
#[cfg(test)]
mod progress_refresh_tests;
#[cfg(test)]
mod tests;
