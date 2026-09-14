use crate::history::source_diff::{
    DeclarationRelation, FingerprintFact, FingerprintLines, correspond_fingerprints,
    diff_fingerprints, fingerprint_lines,
};
use crate::identity::{ByteSpan, ContentRevision};
use crate::store::{PublicationObservations, Store, encode_path};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Deserialize, Serialize)]
pub(super) struct SessionSnapshot {
    root: String,
    pub generation: i64,
    pub index_epoch: String,
    pub capture_start: u64,
    pub capture_end: u64,
    coverage: Value,
    files: BTreeMap<String, SessionFile>,
}

#[derive(Deserialize, Serialize)]
struct SessionFile {
    revision: Option<ContentRevision>,
    status: String,
    facts: Vec<SessionFact>,
    lines: FingerprintLines,
}

/// Duplicate declarations and occurrences retain independent original-byte spans.
#[derive(Deserialize, Serialize)]
struct SessionFact {
    category: String,
    #[serde(flatten)]
    fact: FingerprintFact,
}

pub(super) fn capture(store: &mut Store, capacity: u64) -> Result<SessionSnapshot> {
    ensure!(capacity >= 1024, "session baseline capacity exhausted");
    store.index()?;
    let transaction = store.conn.unchecked_transaction()?;
    let publication = store
        .publication()?
        .ok_or_else(|| anyhow::anyhow!("session publication unavailable"))?;
    let mut snapshot = SessionSnapshot {
        root: encode_path(&store.root),
        generation: store.generation()?,
        index_epoch: publication.index_epoch,
        capture_start: publication.capture_started_ms,
        capture_end: publication.capture_completed_ms,
        coverage: serde_json::to_value(store.coverage()?)?,
        files: BTreeMap::new(),
    };
    let mut consumed = 1024_u64;
    let mut files =
        transaction.prepare("SELECT id,path,revision,status FROM files ORDER BY path")?;
    let mut facts = transaction.prepare(
        "SELECT 'declaration',name,kind,container,start,end FROM definitions WHERE file_id=?1
         UNION ALL SELECT 'occurrence',name,role,NULL,start,end FROM occurrences WHERE file_id=?1
         ORDER BY 5,6,1",
    )?;
    let rows = files.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (id, path, revision, status) = row?;
        let bytes: Vec<u8> = match revision.as_ref() {
            Some(revision) => transaction.query_row(
                "SELECT bytes FROM contents WHERE revision=?1",
                [revision],
                |row| row.get(0),
            )?,
            None => Vec::new(),
        };
        let count: i64 = transaction.query_row("SELECT (SELECT count(*) FROM definitions WHERE file_id=?1)+(SELECT count(*) FROM occurrences WHERE file_id=?1)", [id], |row| row.get(0))?;
        let line_count = bytes.iter().filter(|&&byte| byte == b'\n').count() + 1;
        ensure!(
            consumed
                .saturating_add(count as u64 * 256)
                .saturating_add(line_count as u64 * 80)
                <= capacity,
            "session baseline capacity exhausted"
        );
        let mut file = SessionFile {
            revision: revision
                .as_deref()
                .map(ContentRevision::parse)
                .transpose()?,
            status,
            facts: Vec::with_capacity(count as usize),
            lines: fingerprint_lines(&bytes),
        };
        let mut rows = facts.query([id])?;
        while let Some(row) = rows.next()? {
            let category: String = row.get(0)?;
            let name: String = row.get(1)?;
            let kind: String = row.get(2)?;
            let container: Option<String> = row.get(3)?;
            let start = row.get::<_, i64>(4)? as usize;
            let end = row.get::<_, i64>(5)? as usize;
            let content = bytes
                .get(start..end)
                .ok_or_else(|| anyhow::anyhow!("invalid baseline occurrence span"))?;
            let key = serde_json::to_vec(&(category.as_str(), name, kind, container))?;
            file.facts.push(SessionFact {
                category,
                fact: FingerprintFact {
                    key: ContentRevision::of(&key),
                    span: ByteSpan::new(start, end)?,
                    fingerprint: ContentRevision::of(content),
                },
            });
        }
        consumed = consumed
            .saturating_add(serde_json::to_vec(&file)?.len() as u64 + path.len() as u64 + 8);
        ensure!(consumed <= capacity, "session baseline capacity exhausted");
        snapshot.files.insert(path, file);
    }
    drop(facts);
    drop(files);
    transaction.commit()?;
    Ok(snapshot)
}

pub(super) fn compare(
    before: &SessionSnapshot,
    after: &SessionSnapshot,
    observations: &PublicationObservations,
) -> Result<Value> {
    ensure!(before.root == after.root, "session belongs to another root");
    let renames = exact_renames(before, after);
    let renamed_paths: HashSet<_> = renames.values().copied().collect();
    let mut changes = Vec::with_capacity(
        before
            .files
            .len()
            .saturating_add(after.files.len())
            .min(128),
    );
    let mut total_changes = 0;
    for (path, old) in &before.files {
        if after
            .files
            .get(path)
            .is_some_and(|new| old.revision == new.revision && old.status == new.status)
        {
            continue;
        }
        total_changes += 1;
        if changes.len() >= 128 {
            continue;
        }
        if let Some(&after_path) = renames.get(path.as_str()) {
            changes.push(json!({"path":path,"after_path":after_path,"category":"renamed","before_revision":old.revision,"after_revision":after.files[after_path].revision,"source_changes":{"changes":[],"truncated":0}}));
            continue;
        }
        match after.files.get(path) {
            Some(new) if old.revision == new.revision && old.status == new.status => {}
            Some(new) => {
                let category =
                    if old.revision.is_none() || new.revision.is_none() || old.status != new.status
                    {
                        "coverage_change"
                    } else {
                        "modified"
                    };
                changes.push(json!({"path":path,"category":category,"before_revision":old.revision,"after_revision":new.revision,"before_status":old.status,"after_status":new.status,"occurrences":fact_changes(old,new)?,"source_changes":source_changes(old,new)?}));
            }
            None => {
                let category = observations
                    .changes
                    .iter()
                    .rev()
                    .find(|change| change.path == *path && change.after_revision.is_none())
                    .map_or("uncertain_absence", |change| match change.kind.as_str() {
                        "deleted" => "deleted",
                        "ignored_or_excluded" => "ignored_or_excluded",
                        _ => "uncertain_absence",
                    });
                changes.push(json!({"path":path,"category":category,"before_revision":old.revision,"after_revision":null,"symbol_deletion_proven":false}));
            }
        }
    }
    for (path, file) in &after.files {
        if !before.files.contains_key(path) && !renamed_paths.contains(path.as_str()) {
            total_changes += 1;
            if changes.len() >= 128 {
                continue;
            }
            changes.push(json!({"path":path,"category":"addition_without_preimage","before_revision":null,"after_revision":file.revision}));
        }
    }
    let mut report_bytes = 2;
    let mut retained = 0;
    for change in &changes {
        report_bytes += serde_json::to_vec(change)?.len() + 1;
        if report_bytes > 256 * 1024 {
            break;
        }
        retained += 1;
    }
    changes.truncate(retained);
    Ok(
        json!({"before_generation":before.generation,"after_generation":after.generation,"before_capture_interval":[before.capture_start,before.capture_end],"after_capture_interval":[after.capture_start,after.capture_end],"before_coverage":before.coverage,"after_coverage":after.coverage,"changes":changes,"changes_truncated":total_changes-changes.len(),"intermediate_edits":"not_observed","ownership":"observational_only","symbol_correspondence":"shared histogram line correspondence and occurrence fingerprints; ambiguous matches remain uncertain"}),
    )
}

fn exact_renames<'a>(
    before: &'a SessionSnapshot,
    after: &'a SessionSnapshot,
) -> HashMap<&'a str, &'a str> {
    fn unique_files(snapshot: &SessionSnapshot) -> HashMap<ContentRevision, Option<&str>> {
        let mut files = HashMap::with_capacity(snapshot.files.len());
        for (path, file) in &snapshot.files {
            if let Some(revision) = file.revision {
                files
                    .entry(revision)
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(path.as_str()));
            }
        }
        files
    }
    let before_unique = unique_files(before);
    let after_unique = unique_files(after);
    let candidates = before
        .files
        .keys()
        .filter(|path| !after.files.contains_key(*path))
        .count();
    let mut renames = HashMap::with_capacity(candidates);
    for (revision, path) in before_unique {
        if let (Some(path), Some(Some(after_path))) = (path, after_unique.get(&revision)) {
            if !after.files.contains_key(path) && !before.files.contains_key(*after_path) {
                renames.insert(path, *after_path);
            }
        }
    }
    renames
}

fn source_changes(before: &SessionFile, after: &SessionFile) -> Result<Value> {
    let diff = diff_fingerprints(&before.lines, &after.lines)?;
    Ok(
        json!({"changes":diff.changes.iter().take(128).collect::<Vec<_>>(),"truncated":diff.changes.len().saturating_sub(128)}),
    )
}

fn fact_changes(before: &SessionFile, after: &SessionFile) -> Result<Value> {
    let before_facts: Vec<_> = before.facts.iter().map(|fact| fact.fact.clone()).collect();
    let after_facts: Vec<_> = after.facts.iter().map(|fact| fact.fact.clone()).collect();
    let correspondence = correspond_fingerprints(
        &before_facts,
        &after_facts,
        before.status == "complete",
        after.status == "complete",
        Some(&before.lines),
        Some(&after.lines),
    )?;
    let changed_count = correspondence
        .iter()
        .filter(|row| row.relation != DeclarationRelation::Unchanged)
        .count();
    let mut changes = Vec::with_capacity(changed_count.min(32));
    for row in correspondence
        .iter()
        .filter(|row| row.relation != DeclarationRelation::Unchanged)
        .take(32)
    {
        changes.push(json!({"before":row.before.map(|index|&before.facts[index]),"after":row.after.map(|index|&after.facts[index]),"candidates":row.candidates.iter().take(8).map(|&index|&after.facts[index]).collect::<Vec<_>>(),"candidates_truncated":row.candidates_truncated || row.candidates.len()>8,"correspondence":row.relation,"deletion_proven":row.relation==DeclarationRelation::Deleted}));
    }
    Ok(json!({"changes":changes,"truncated":changed_count-changes.len()}))
}
