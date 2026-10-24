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

## Staged build

Each pass stages into `stage-*/index.sqlite3` with `journal_mode=OFF` and
`synchronous=OFF`; any failure discards the directory. Staged tables are loaded
without secondary indexes, and staged documents are plain rows rather than FTS.
The pass creates the secondary indexes after the scan and before resolution.
Inserts reuse statements compiled once per connection.

## Publication storage

Publication copies staged tables by explicit column names. It replaces the FTS
table with an empty one inside the publication transaction instead of deleting
rows, so each generation's lexical index is built from scratch without delete
markers; FTS5 automerge bounds its segments, and no `merge` or `optimize` runs.

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
