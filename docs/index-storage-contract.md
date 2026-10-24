# Index database lifecycle

`<cache>/index.sqlite3` is a rebuildable cache of the working tree. Publication
semantics are in the [index contract](index-contract.md); this document covers the
database file itself (`src/store/live_index.rs`, `src/store/schema.rs`).

## Schema revision

`PRAGMA user_version` holds `src/store/schema.rs:INDEX_SCHEMA_REVISION`. Bump it
on any table, column, or index change. There are no migrations:

- The writer (`Store::open`) creates the schema in an empty database and stamps
  the revision. A database with tables and another revision, including an
  unstamped one, has every table dropped and is then recreated empty, so the
  next pass rebuilds it at generation 1 with a new index epoch.
- Discarding needs a free writer lease (`index.lock`). While another writer holds
  it, opening answers `index_warming` and leaves the file untouched.
- Readers (`Store::open_read`) answer `index_warming` for another revision.
- A worktree seed stamps its staged database with the same revision
  (`src/store/seed.rs`).

New databases use `auto_vacuum=INCREMENTAL`; a discarded database gets one
`VACUUM` after its tables are dropped, which also returns its old pages.

## Corruption recovery

SQLite `DatabaseCorrupt` or `NotADatabase` on the live index is recovered by
quarantine, never by repair:

- When the writer open fails this way and the writer lease is free, the index
  and its WAL move to `index.corrupt.sqlite3` (and `-wal`), the shared-memory
  file is removed, and an empty index is created. Each quarantine replaces the
  previous one, so at most one corrupt copy is retained.
- When an unbounded pass fails this way and `PRAGMA quick_check` confirms the
  live index is damaged, the pass quarantines it under its lease and returns the
  error; the next pass rebuilds. Bounded slices only report the error.
- A held lease leaves a corrupt file in place; its writer recovers it.

Selected-cache refusals (read-only or unopenable caches) are separate; see
`src/store/cache_access.rs`. Retained checkpoint corruption follows
`src/store/index_checkpoint/checkpoint_storage.rs:is_disposable_stage_error`.

## Unchanged files

`files` records each file's `stat_size`, `stat_mtime_ns`, `stat_ctime_ns`,
`stat_inode`, and `stat_device` (`src/store/scan/file_stat.rs`). A pass first walks
and stats the tree (`src/store/scan/tree_observation.rs`): a file whose stat equals
the recorded one keeps its published revision without being read or hashed. Other
files are read and hashed, holding at most 16 MiB before staging. When every file
matches the publication and the fingerprint is unchanged, the pass stages and
publishes nothing; it only records stats observed for files whose revision stands.

A stat is recorded only when its modification and status-change times are at least
2 s older than the pass start; a racy file is hashed again on the next pass, as in
Git's index. Read errors and files whose length changed while being read record no
stat, and a cancelled extraction is read again. Stat columns are observations, not
index content.

## Staged build

Each pass stages into `stage-*/index.sqlite3` with `journal_mode=OFF` and
`synchronous=OFF`; any failure discards the directory. Staged tables are loaded
without secondary indexes, and staged documents are plain rows rather than FTS.
Staged ids follow walk order exactly as in a full rebuild, because resolvers
order by row id. Every file's facts are staged, reusing the published extraction
for a revision that stands. Bytes, extraction cache rows, and regions are staged
only for files listed in the staged-only `restaged` table: a new or changed
revision, or every file when the live `regions_contract` meta differs from
`src/store/regions.rs:contract`. Published bytes of the other revisions are then
copied from the live index, so resolvers read the complete tree. The pass creates
the secondary indexes after the scan and before resolution. Inserts reuse
statements compiled once per connection.

## Cross-file resolution

Resolution is global: global-name candidates, module and include targets, and PHP
inheritance, XenForo extensions, and parent projections read every file's
definitions and bytes, honor row-id order, and share one derived-fact budget, so an
edit to one file can change rows stored for any other. Each staged pass therefore
runs every resolver over the complete staged tree, exactly as a full rebuild does,
and publication compares results instead of predicting dependents. Tracking
dependents would duplicate each resolver's lookup rules and drift with them.

## Publication storage

An index holding no files receives every staged row by explicit column names,
ids included. Otherwise publication diffs (`src/store/publish/publication_diff.rs`):

- Files pair by path; a path keeps its `files.id`, and an added path gets a fresh id.
- A file whose definition sequence is unchanged keeps its definition ids. Otherwise
  definitions match by name, kind, container, and order among equals: a match keeps
  its id and takes the new span, the rest are inserted or deleted. References into
  an edited file therefore stay valid, and within a file row order need not follow
  source order; readers order definitions by position.
- Occurrences and relationships compare as per-file row sets, with definition ids
  translated to live ids; a differing file's rows are deleted by `file_id` and
  reinserted.
- Regions and their FTS documents of restaged and removed files are deleted by
  rowid and reinserted under fresh ids.
- Bytes are added for restaged revisions and dropped once no file references them;
  extraction cache rows keep exactly the current version's rows for current files.

The result is the full rebuild's rows up to row ids, stat columns, and row order.
Foreign keys are off inside the transaction: children are deleted before parents,
and per-row parent probes would scan unindexed relationship columns. FTS5 delete
markers are bounded by a `merge` of 8 pages per changed document (at least 64) on
each publication, or an `optimize` when a quarter or more of the documents changed.

After a committed publication the writer reclaims space, best effort:
`PRAGMA incremental_vacuum` returns free pages to the filesystem, then
`wal_checkpoint(TRUNCATE)` runs without waiting on its busy handler. A reader on
an older snapshot leaves the WAL in place; `journal_size_limit` (64 MiB) trims it
at a later reset. A bounded slice skips reclamation once its deadline expires.

## Connections

| Connection | Journal | Sync | Page cache | Map |
| --- | --- | --- | --- | --- |
| Writer | WAL | NORMAL | 8 MiB | none |
| Reader | WAL (read-only, `query_only`) | n/a | 2 MiB | 256 MiB |
| Staged build | OFF | OFF | 4 MiB | none |

`synchronous=NORMAL` can lose the last publication on power loss but never
corrupts the WAL; the next reconcile republishes it.

## Extraction cache identity

Durable extractions key on `src/store/scan.rs:extraction_cache_version`: the
extraction contract revision plus the locked versions of every `tree-sitter*`
crate, which `build.rs` reads from `Cargo.lock` into
`TRUFFLEPIG_GRAMMAR_VERSIONS`. Other dependency changes keep cached facts. Bump
`EXTRACTION_CONTRACT_REVISION` for any extractor change.
