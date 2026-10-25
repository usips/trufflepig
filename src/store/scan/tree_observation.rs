//! A pass's first phase: walk the tree, stat every eligible file, and reuse the
//! published revision of each file whose recorded stat still matches. Other
//! files are read and hashed, up to a memory budget, so an unchanged tree is
//! recognized before anything is staged.
use super::file_stat::{FileStat, PassStart};
use super::read_source;
use super::scan_tally::ScanTally;
use crate::store::{Coverage, index_deadline};
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tp_core::path_codec::encode_path;
use tp_core::query_deadline::QueryDeadline;

/// Bytes a pass may hold from files read before staging; later files are read while staging.
const OBSERVATION_READ_BUDGET: usize = 16 * 1024 * 1024;

/// One file row of the previous publication.
pub(in crate::store) struct PublishedFileRecord {
    pub(in crate::store) revision: Option<String>,
    pub(in crate::store) status: String,
    pub(in crate::store) bytes: u64,
    stat: Option<FileStat>,
    has_contents: bool,
}

/// How a pass obtains a file's bytes.
pub(in crate::store) enum FileSource {
    /// The recorded stat matches, so the published revision stands unread.
    Published,
    /// Read and hashed this pass.
    Read(Vec<u8>),
    /// Read attempted; excluded or unreadable.
    Unreadable,
    /// Not read yet; staging reads it.
    Pending,
}

pub(in crate::store) struct ObservedFile {
    pub(in crate::store) path: String,
    pub(in crate::store) relative: PathBuf,
    pub(in crate::store) read_status: &'static str,
    pub(in crate::store) length: u64,
    pub(in crate::store) revision: Option<String>,
    pub(in crate::store) source: FileSource,
    /// The stat to record; absent when racy or unreadable.
    pub(in crate::store) stat: Option<FileStat>,
    pub(in crate::store) published: Option<PublishedFileRecord>,
}

pub(in crate::store) struct TreeObservation {
    pub(in crate::store) files: Vec<ObservedFile>,
    pub(in crate::store) walk_failures: usize,
    removed_paths: usize,
}

impl TreeObservation {
    /// Walks `root` in path order, skipping `cache` and pruned build/VCS trees.
    pub(in crate::store) fn observe(
        previous: &Connection,
        root: &Path,
        cache: &Path,
        deadline: Option<&QueryDeadline>,
    ) -> Result<Self> {
        let pass_start = PassStart::now();
        let mut published = published_records(previous)?;
        let read_ahead = !published.is_empty();
        let mut budget = OBSERVATION_READ_BUDGET;
        let mut files = Vec::with_capacity(published.len() + published.len() / 8 + 16);
        let mut walk_failures = 0;
        for entry in tree_walk(root, cache).build() {
            if let Some(deadline) = deadline {
                index_deadline::check(deadline)?;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    walk_failures += 1;
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let relative = entry.path().strip_prefix(root)?.to_path_buf();
            let path = encode_path(&relative);
            let previous = published.remove(&path);
            let mut file = ObservedFile {
                path,
                relative,
                read_status: "metadata_error",
                length: 0,
                revision: None,
                source: FileSource::Unreadable,
                stat: None,
                published: previous,
            };
            match root.join(&file.relative).symlink_metadata() {
                Ok(metadata) => {
                    let stat = FileStat::of(&metadata);
                    if !file.reuse_published(stat) {
                        file.stat = (!stat.is_racy(pass_start)).then_some(stat);
                        file.length = metadata.len();
                        file.source = FileSource::Pending;
                        if read_ahead && budget as u64 >= metadata.len() {
                            file.read(root, &metadata);
                            if let FileSource::Read(source) = &file.source {
                                budget -= source.len();
                            }
                        }
                    }
                }
                Err(_) => file.source = FileSource::Unreadable,
            }
            files.push(file);
        }
        Ok(Self {
            files,
            walk_failures,
            removed_paths: published.len(),
        })
    }

    /// The coverage and fingerprint the previous publication would record for
    /// this tree, when every file matches it; `None` when anything differs.
    pub(in crate::store) fn unchanged_tally(&self) -> Option<(Coverage, String)> {
        if self.removed_paths > 0 {
            return None;
        }
        let mut tally = ScanTally::new();
        for file in &self.files {
            let published = file.published.as_ref()?;
            let matches = match file.source {
                FileSource::Published => true,
                FileSource::Read(_) | FileSource::Unreadable => {
                    published.revision == file.revision
                        && published.bytes == file.length
                        && (file.revision.is_some() || published.status == file.read_status)
                }
                FileSource::Pending => false,
            };
            if !matches || published.status == "cancelled" {
                return None;
            }
            tally.file(
                &file.path,
                file.read_status,
                file.length,
                file.revision.as_deref(),
                &published.status,
                file.revision.is_some(),
            );
        }
        Some(tally.finish(self.walk_failures))
    }

    /// Newly observed stats of files whose published revision stands.
    pub(in crate::store) fn stat_refresh(&self) -> StatRefresh {
        let mut refresh = StatRefresh::default();
        for file in &self.files {
            refresh.note(file);
        }
        refresh
    }
}

/// Stats to record for files whose published revision stands, so later passes
/// skip reading them; recording them never changes a publication.
#[derive(Default)]
pub(in crate::store) struct StatRefresh {
    entries: Vec<(String, Option<String>, FileStat)>,
}

impl StatRefresh {
    pub(in crate::store) fn note(&mut self, file: &ObservedFile) {
        if let (Some(stat), Some(published)) = (file.stat, file.published.as_ref())
            && published.stat != Some(stat)
            && published.revision == file.revision
        {
            self.entries
                .push((file.path.clone(), file.revision.clone(), stat));
        }
    }

    pub(in crate::store) fn apply(&self, live: &Connection) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let transaction = live.unchecked_transaction()?;
        for (path, revision, stat) in &self.entries {
            let [size, mtime, ctime, inode, device] = FileStat::columns(Some(*stat));
            transaction
                .prepare_cached(
                    "UPDATE files SET stat_size=?1,stat_mtime_ns=?2,stat_ctime_ns=?3,stat_inode=?4,stat_device=?5
                     WHERE path=?6 AND revision IS ?7",
                )?
                .execute(params![size, mtime, ctime, inode, device, path, revision])?;
        }
        transaction.commit()?;
        Ok(())
    }
}

impl ObservedFile {
    /// Takes the published revision when the recorded stat still matches.
    fn reuse_published(&mut self, stat: FileStat) -> bool {
        let Some(published) = self.published.as_ref() else {
            return false;
        };
        let read_status = match (&published.revision, published.status.as_str()) {
            (Some(_), _) if published.has_contents => "read",
            (None, "resource_excluded") => "resource_excluded",
            (None, "binary_excluded") => "binary_excluded",
            _ => return false,
        };
        if published.stat != Some(stat) || published.status == "cancelled" {
            return false;
        }
        self.read_status = read_status;
        self.length = published.bytes;
        self.revision = published.revision.clone();
        self.stat = Some(stat);
        self.source = FileSource::Published;
        true
    }

    /// Reads and hashes the file; keeps the stat only if the length still agrees.
    pub(in crate::store) fn read(&mut self, root: &Path, metadata: &std::fs::Metadata) {
        let (source, status, length) = read_source(root, &self.relative, metadata);
        if !matches!(status, "read" | "resource_excluded" | "binary_excluded")
            || length != metadata.len()
        {
            self.stat = None;
        }
        self.read_status = status;
        self.length = length;
        self.revision = source
            .as_ref()
            .map(|source| blake3::hash(source).to_hex().to_string());
        self.source = match source {
            Some(source) => FileSource::Read(source),
            None => FileSource::Unreadable,
        };
    }

    /// Reads a pending file during staging.
    pub(in crate::store) fn read_pending(&mut self, root: &Path) {
        match root.join(&self.relative).symlink_metadata() {
            Ok(metadata) => self.read(root, &metadata),
            Err(_) => {
                self.read_status = "metadata_error";
                self.length = 0;
                self.stat = None;
                self.source = FileSource::Unreadable;
            }
        }
    }
}

fn published_records(previous: &Connection) -> Result<HashMap<String, PublishedFileRecord>> {
    let count: i64 = previous.query_row("SELECT count(*) FROM files", [], |row| row.get(0))?;
    let mut records = HashMap::with_capacity(count.try_into()?);
    let mut statement = previous.prepare(
        "SELECT f.path,f.revision,f.status,f.bytes,f.stat_size,f.stat_mtime_ns,f.stat_ctime_ns,
                f.stat_inode,f.stat_device,c.revision IS NOT NULL
         FROM files f LEFT JOIN contents c ON c.revision=f.revision",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        records.insert(
            row.get(0)?,
            PublishedFileRecord {
                revision: row.get(1)?,
                status: row.get(2)?,
                bytes: row.get::<_, i64>(3)?.try_into()?,
                stat: FileStat::from_row(row, 4)?,
                has_contents: row.get(9)?,
            },
        );
    }
    Ok(records)
}

/// The eligible-file walk: sorted, hidden files included, `.git` and build trees pruned.
fn tree_walk(root: &Path, cache: &Path) -> ignore::WalkBuilder {
    let excluded_cache = cache.to_path_buf();
    let mut walker = ignore::WalkBuilder::new(root);
    walker
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b));
    walker.filter_entry(move |entry| {
        if entry.path().starts_with(&excluded_cache) {
            return false;
        }
        // A linked worktree's `.git` is a file; skip it whatever its type.
        entry.file_name() != ".git"
            && (!entry.file_type().is_some_and(|kind| kind.is_dir())
                || !matches!(
                    entry.file_name().to_str(),
                    Some("target" | "node_modules" | ".trufflepig")
                ))
    });
    walker
}
