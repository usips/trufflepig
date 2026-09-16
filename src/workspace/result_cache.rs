//! Bounded workspace snapshots retain immutable entries independently of member caches.
use super::config::Member;
mod lines;
use crate::{
    identity::{ResultCursor, ResultHandle},
    output::{OutputBudget, OutputFormat},
    results::{self, ResultEntry},
    store::{Store, decode_path, encode_path},
};
use anyhow::{Context, Result, bail, ensure};
use lines::{compact_coverage, member_coverage_summary};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

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
}
impl MemberSnapshot {
    pub fn capture(member: &Member, store: &Store, cache: &Path, generation: i64) -> Result<Self> {
        member.verify_identity()?;
        let identity = member
            .identity
            .as_ref()
            .context("member_unavailable: original root identity is missing")?;
        let index_identity =
            store
                .conn
                .query_row("SELECT value FROM meta WHERE key='index_epoch'", [], |r| {
                    r.get(0)
                })?;
        Ok(Self {
            name: member.name.clone(),
            root: encode_path(&member.root),
            cache: encode_path(&cache.canonicalize()?),
            device: identity.device,
            inode: identity.inode,
            index_identity,
            generation,
            coverage: serde_json::to_value(store.coverage()?)?,
        })
    }
    pub fn open(&self, config: &super::config::WorkspaceConfig) -> Result<Store> {
        let member = config
            .members
            .iter()
            .find(|member| member.name == self.name)
            .context("member_unavailable: result owner was removed from workspace")?;
        ensure!(
            encode_path(&member.root) == self.root,
            "member_unavailable: result owner path changed"
        );
        let metadata = member
            .root
            .metadata()
            .context("member_unavailable: result owner is missing")?;
        ensure!(
            metadata.dev() == self.device
                && metadata.ino() == self.inode
                && member.root.canonicalize()? == member.root,
            "member_unavailable: result owner was replaced"
        );
        let cache = decode_path(&self.cache)?;
        ensure!(
            cache.join("index.sqlite3").is_file(),
            "member_unavailable: owning index was removed"
        );
        let store = Store::open(&member.root, &cache)?;
        let current: Option<String> = store
            .conn
            .query_row("SELECT value FROM meta WHERE key='index_epoch'", [], |r| {
                r.get(0)
            })
            .optional()?;
        ensure!(
            current.as_ref() == Some(&self.index_identity),
            "member_unavailable: owning index was replaced"
        );
        Ok(store)
    }
    pub fn metadata(&self, workspace: &str) -> Value {
        json!({"workspace":workspace,"member":self.name,"repository":self.root})
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OwnedEntry {
    pub owner: usize,
    pub member_rank: usize,
    pub entry: ResultEntry,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceSet {
    pub workspace: String,
    pub home: Option<String>,
    pub owners: Vec<MemberSnapshot>,
    pub coverage: Vec<Value>,
    pub hits: Vec<OwnedEntry>,
    pub truncated: bool,
}
pub struct WorkspaceResults {
    pub(super) conn: Connection,
}
impl WorkspaceResults {
    pub fn open(cache: &Path) -> Result<Self> {
        std::fs::create_dir_all(cache)?;
        std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
        let conn = Connection::open(cache.join("workspace.sqlite3"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA journal_size_limit=4194304; CREATE TABLE IF NOT EXISTS workspace_results(id TEXT PRIMARY KEY, expires INTEGER NOT NULL, payload TEXT NOT NULL)")?;
        Ok(Self { conn })
    }
    pub fn save(&self, mut set: WorkspaceSet) -> Result<String> {
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
            "result_cache_unavailable: workspace metadata exceeds capacity"
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM workspace_results WHERE expires<=?1",
            [results::now()],
        )?;
        tx.execute(
            "INSERT INTO workspace_results VALUES(?1,?2,?3)",
            params![id, results::now() + 600, payload],
        )?;
        loop {
            let (count, bytes): (usize, usize) = tx.query_row("SELECT count(*),coalesce(sum(length(CAST(payload AS BLOB))),0) FROM workspace_results", [], |r| Ok((r.get::<_,i64>(0)? as usize,r.get::<_,i64>(1)? as usize)))?;
            if count <= 50 && bytes <= results::MAX_BYTES {
                break;
            }
            tx.execute("DELETE FROM workspace_results WHERE id=(SELECT id FROM workspace_results WHERE id<>?1 ORDER BY expires,id LIMIT 1)", [&id])?;
        }
        tx.commit()?;
        Ok(id)
    }
    pub fn load(&self, id: &str) -> Result<WorkspaceSet> {
        uuid::Uuid::parse_str(id).context("invalid_handle: invalid set identifier")?;
        let payload: Option<String> = self
            .conn
            .query_row(
                "SELECT payload FROM workspace_results WHERE id=?1 AND expires>?2",
                params![id, results::now()],
                |r| r.get(0),
            )
            .optional()?;
        let payload = payload
            .context("expired_result: workspace result expired or was evicted; search again")?;
        ensure!(
            payload.len() <= results::MAX_BYTES,
            "result_unavailable: oversized workspace result"
        );
        let set: WorkspaceSet = serde_json::from_str(&payload)
            .context("result_unavailable: invalid workspace result")?;
        ensure!(
            set.owners.len() <= 32
                && set.hits.len() <= results::MAX_HITS
                && set.hits.iter().all(|hit| hit.owner < set.owners.len()),
            "result_unavailable: invalid workspace ownership"
        );
        Ok(set)
    }
    pub fn entry(&self, handle: &str) -> Result<(WorkspaceSet, usize)> {
        let handle: ResultHandle = handle.parse()?;
        let set = self.load(&handle.set.simple().to_string())?;
        ensure!(
            handle.ordinal <= set.hits.len(),
            "invalid_handle: ordinal outside result set"
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
        let set = self.load(id)?;
        ensure!(
            offset <= set.hits.len(),
            "invalid_cursor: offset outside result set"
        );
        let available = set.hits.len() - offset;
        let max_count = available.min(limit).min(budget.limit);
        let coverage_line = member_coverage_summary(&set.coverage);
        let render = |count: usize, names: bool| -> Result<String> {
            match budget.format {
                OutputFormat::Json => budget.encode(&page_value(&set, id, offset, count, names)?),
                OutputFormat::Lines => Ok(results::lines::page_text(
                    set.hits[offset..offset + count]
                        .iter()
                        .map(|owned| (Some(set.owners[owned.owner].name.as_str()), &owned.entry)),
                    names,
                    &coverage_line,
                    results::lines::next_cursor(id, offset, count, set.hits.len()).as_deref(),
                    set.truncated,
                )),
            }
        };
        for names in [false, true] {
            let mut low = 0;
            let mut high = max_count;
            while low < high {
                let count = low + (high - low).div_ceil(2);
                if budget.fits(&render(count, names)?) {
                    low = count;
                } else {
                    high = count - 1;
                }
            }
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
        bail!("budget_too_small: workspace coverage and one result do not fit");
    }
}

fn page_value(
    set: &WorkspaceSet,
    id: &str,
    offset: usize,
    count: usize,
    names: bool,
) -> Result<Value> {
    let mut hits = Vec::with_capacity(count);
    for owned in &set.hits[offset..offset + count] {
        let owner = &set.owners[owned.owner];
        let root = decode_path(&owner.root)?;
        let mut entry = results::compact_entry(&owned.entry, &root, Some(&owner.name), names)?;
        if entry.is_object() {
            entry["member"] = owner.name.clone().into();
            if !matches!(&owned.entry, ResultEntry::LiveSource(_)) {
                entry["member_rank"] = owned.member_rank.into();
            }
        }
        hits.push(entry);
    }
    let next = (offset + count < set.hits.len()).then(|| format!("{id}@{}", offset + count));
    Ok(json!({
        "workspace": set.workspace,
        "home": set.home,
        "coverage": compact_coverage(&set.coverage),
        "hits": hits,
        "next": next,
        "truncated": set.truncated,
        "tokenizer": "o200k_base"
    }))
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
mod tests;
