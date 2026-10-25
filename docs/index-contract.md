# Authoritative index and updates

## Storage and identity

SQLite WAL with FTS5 (`<cache>/index.sqlite3`) holds source revisions, definitions,
occurrences, relationships, and lexical documents. Immutable result sets use a separate
`<cache>/results.sqlite3` (`crates/tp-index/src/results/result_set_store.rs`), so saving query results
never waits behind publication; the file's revision, recovery, and space are in the
[storage lifecycle](index-storage-contract.md). Index-backed reads preflight the
selected writable result cache before parent lookup. Occurrence identity is
root/path/revision/span; content identity supports reuse without merging distinct
definitions or overrides.

Ordinary indexes retain exact identifiers. Separate FTS columns contain expanded
snake/camel identifiers, body text, and path terms. All eligible file content is
covered by bounded source regions, preferably on syntax boundaries with the
containing symbol attached. File records expose resource exclusions.
[FTS5 reference](https://www.sqlite.org/fts5.html)

## File ranking

Ordinary queries collapse each retrieval lane to its best region per file and fuse
lanes by weighted reciprocal rank; the [ranking contract](ranking-contract.md)
defines lanes, weights, evidence tiers, and the path prior. `sym:` and `re:` retain
occurrences. Pages maximize file references before adding optional symbol names.

## Coherent publication

1. Reconcile the working tree, including untracked eligible files. A file whose recorded [stat](index-storage-contract.md#unchanged-files)
   still matches keeps its published revision unread; content hashing establishes every other revision.
2. Read changed files into bounded buffers; a tree matching the publication stages nothing.
   Otherwise one worker stages the complete current file set, reusing durable extraction only
   when path, revision, source status, byte length, and extraction version match; cancelled
   extraction is retried. Resource bounds and failures remain visible.
3. Build the staged symbol universe and recompute relationship resolution,
   including references in unchanged callers whose targets changed.
4. Publish the rows of added, removed, and differing files in one transaction
   ([diff publication](index-storage-contract.md#publication-storage)), then advance the generation.
5. Roll back every published component if the transaction fails. Prune
   superseded content without discarding unexpired handle revision metadata.

Each published generation records a capture interval and per-file revisions.
It is a reconciled observation, not an atomic filesystem snapshot. Publication
observations distinguish edits, additions, deletions, and coverage changes;
consumers receive an explicit incomplete window after observation retention
limits are exceeded. The observation journal retains at most 256 generation
windows, 4,096 observations, and 4 MiB of observation payloads. Sessions persist
their own baseline fingerprints outside the replaceable live index; see [diagnostics](diagnostics-contract.md).

An exclusive writer lease covers staging through publication. A checkpoint resumes
only for a matching root, index contract, base epoch, and generation; mismatches reset (`crates/tp-index/src/store/index_checkpoint.rs`). A bounded slice commits its new extractions in batches of 250 ms or 256 files and at expiry; extractions served by the published index are reused, not copied. An unbounded reconcile reads retained extractions and writes none.

Parsing cancellation retains lexical coverage and a parse-failure status. It
publishes no structural facts represented as freshly parsed. The parser is reset
before a different input. Partially recovered facts from a completed parse are
separately labeled and cannot strengthen resolver certainty.

Only committed generations are queryable. A reader holds one snapshot through
candidate retrieval and graph joins. Readers are query-only
(`crates/tp-index/src/store/read_access.rs:open_read`): they open the index read-only, run no
schema or metadata writes, wait at most 2 s on a lock, and are interrupted when
the request's 20 s query deadline (`crates/tp-core/src/query_deadline.rs`) expires, answering
`timed_out`; inline work uses that original deadline without pausing it. A missing
index or generation 0 answers `index_warming`. Only indexing, sessions, and the
reconciler open the writer (`Store::open`). A reader
may read a linked worktree's current bytes through its parent member's index
(root-relative paths address the same files); its result sets are saved in the
worktree's own cache, and `show` verifies those bytes against indexed revisions.
Only a worktree sharing the member's Git common directory reads this way. Files
that may differ are worktree changes against the parent's `HEAD` (committed,
staged, unstaged, untracked) plus the parent's uncommitted and untracked files;
each Git probe is bounded to 2 s and a result is reused for 5 s
(`src/workspace/home_index/worktree_divergence.rs`). A hit's file also differs
when its worktree bytes no longer hash to the indexed revision; one query
hashes each file once and at most 128 files (`home_index/hit_verification.rs`).
Plain and `sym:` searches re-run normal retrieval against at most 64 changed
files without publishing an index. Parent fallback also examines divergent
files and refreshes discovery on a miss; own-index reads verify returned
candidates (`crates/tp-index/src/search/current_candidate_reads.rs`). Removed matches disappear;
current references, lexical evidence, and snippets retain current coordinates.
Unchecked or unextractable paths make coverage partial and truncated. Stale
semantic candidates become current-source representatives with semantic coverage
marked unverified. Current-byte hits never differ; re-read files count as differing.
`show sym:`, `map FILE`, and `show HANDLE` re-read divergent files (a file hit whole);
failed extraction can serve parent bytes with `verified: false` and `source: parent_index`.
Through a root's own index, a stale `sym:` read waits up to 3 s for a daemon
republish, then answers from current bytes with `freshness: unpublished`
(`crates/tp-index/src/source/acquisition/stale_symbol.rs`). Inline prep uses at most 2 s per
root/member, reserving 250 ms from the original read deadline. A published root or
verified parent view remains usable while the writer lease is held. Three consecutive eligible full
slices that expire without saving new durable file facts report `maintenance_required` with `index`/`init` guidance
(`crates/tp-index/src/store/index_deadline.rs`).

## Semantic preparation

Each canonical root has independent preparation state. A preparation request
captures one committed generation, reads its bounded source regions, and derives
content keys before submitting missing vectors to the shared per-user inference
worker. Completions are stored by content key and joined to occurrences only
from the captured generation; a later publication cannot receive a stale
completion. Renames can reuse vectors while occurrence identity changes.

Search reads source vectors already present in the root cache. It never embeds
candidate regions or queues background work on the query path. Missing vectors,
worker failures, and the 500 ms query deadline produce explicit semantic
coverage and lexical fallback. Preparation status reports the requested
generation, progress, cached inputs, missing inputs, and failures.

## Daemon and invalidation

Each canonical root has one daemon, protected by an exclusive startup lock and
a length-prefixed JSON Unix socket protocol. Worktrees keep separate databases,
seeded from the member's (or main checkout's) cache as a warm start
(`crates/tp-index/src/store/seed.rs`): the seed copies content-addressed extraction facts and
embeddings but never a publication, so the first reconcile publishes generation 1
for the worktree root. Workspace parent reads use the configured member cache under
the selected workspace base; standalone parent reads use the main checkout's default
cache even when explicit `--cache` holds worktree results and divergence
([workspace](workspace-contract.md#retrieval-and-output)). A seed attempt that does
work is recorded in the worktree cache's `seed-outcome.json`, which `ws status`
reports. A linked worktree's `.git` file is never indexed as source. A per-root daemon exits on its own when its root
disappears, and the router evicts the cache (see [cli](cli.md#cache-and-daemon)).
A stale socket does not permit a second writer while the startup lock is held.
Requests carry client-created UUIDs and explicit session/client context.
Semantic inference uses a separate per-user worker and lease; root daemons do
not own model sessions.
The [history worker](history-contract.md) shares immutable Git facts across
linked worktrees through a separate database, keyed by canonical common directory.
Live and history publication have independent transaction boundaries.

A per-user system daemon routes requests to the owning workspace coordinator or
per-root daemon as a single endpoint agent harnesses can allowlist; see
`src/system.rs`. It runs no watcher and no index: it starts the target daemon
when missing and proxies the reply. Its socket lives in a per-user runtime
directory (`crates/tp-core/src/runtime_paths.rs:system_dir`). Clients treat connect errors `NotFound`,
`ConnectionRefused`, and `PermissionDenied` as "no daemon" and fall back
(`crates/tp-core/src/daemon_transport.rs:unreachable`), first to the router's file spool
(`crates/tp-core/src/daemon_transport/spool.rs`, directory `crates/tp-core/src/runtime_paths.rs:spool_dir`), which carries the
same JSON request and reply bodies through atomically renamed files and is served
by the router's workers. The frame protocol and its limits apply unchanged per
hop; concurrency is in the [runtime contract](runtime-contract.md).

The daemon reconciles at startup, then every five minutes while a watcher is
active or every 30 seconds without one. Watch events are hints that accelerate
reconciliation; events only inside `.git`, `target`, `node_modules`, or
`.trufflepig` are ignored, and overflow, watch exhaustion, and missed events
trigger recovery. After the initial reconcile, a watching daemon's maintenance
thread runs at nice 10 with idle I/O priority; its request workers do not. Events use a 75 ms quiet period with a one-second maximum debounce; no
sub-debounce visibility promise applies. The recursive watcher covers the root
and filters cache events, while the indexing walk prunes ignored/build
directories. Ignored trees may still consume operating-system watches.

Reconciliation observes ignore rules and relevant configuration/include bytes; module
candidate resolution runs against the staged files. Cached facts follow the [extraction
cache identity](index-storage-contract.md#extraction-cache-identity). Each staged pass
re-resolves the full reference universe ([why](index-storage-contract.md#cross-file-resolution)); only publication writes are proportional to changed rows.

Keep only the current graph. `refs`, `ctx`, and `map` expose structural facts;
a module map does not require PageRank. Readers do not join old result identities
to graph nodes reused by a replacement generation.

## Resource lifecycle

SQLite page caches are in the [storage lifecycle](index-storage-contract.md#connections).
Indexing reads at most 2 MiB per source file, retains at most 50,000 extracted facts per
file, and divides lexical coverage into regions of at most 4,096 raw bytes.
Sources containing NUL or exceeding the read cap remain file records with an
exclusion reason. General current-source reads have a separate 16 MiB cap.
Stage files live in the selected cache; completion clears progress and removes them, while interruptions may retain a valid checkpoint. The next writer lease holder or [router sweep](runtime-contract.md#cache-and-daemon) removes other stale stages.
Progress (`scanning`, `finalizing`, `maintenance_required`) shows completed/reused counts and `index` guidance; status/pages/`more` refresh it from the verified root and selected cache.
Saved results omit phases; refresh is read-only and skips seed/index (`src/workspace/result_cache/index_progress_source.rs`, `crates/tp-index/src/results/page.rs`, `src/workspace/result_cache.rs`).
Result and embedding retention limits are in the [retrieval](retrieval-contract.md) and
[semantic](semantic-contract.md) contracts. Measure cold and warm cost before changing
storage architecture.

The socket is `daemon.sock` inside the per-root cache; a short explicit cache
path avoids Unix socket path-length limits. Requests carry a
64 KiB/256-argument cap and a 4 MiB reply cap. There is no protocol version or build
handshake yet; stop the daemon before replacing its binary. Automatic startup
spawns a child process and falls back to coherent local access if needed; it does
not establish detached service lifecycle guarantees.

`--no-daemon` uses no background process. Index-backed reads may use bounded inline
preparation; explicit foreground `index`/`init` finish indexing. Semantic search uses cached vectors only;
`semantic prepare` acquires the root lease and runs locally. Metadata/status do not reconcile.

## Bounded diagnostics probes

`doctor` checks SQLite structure, foreign keys, FTS integrity, unchanged-source extraction
identity, semantic cache provenance, and accidental model initialization. SQL checks have a
100 ms progress deadline and a 10 ms busy wait. Extraction samples at most four source files
of at most 64 KiB, with a 250 ms inter-file cutoff; a running extraction may finish after
that cutoff. Semantic checks sample at most sixteen vectors without loading a model.

Outcomes distinguish passed, failed, incomplete, and unavailable checks. Source drift is
reported separately from corruption; unverified cached facts are not silently treated as
valid. The daemon attempts these checks on idle ticks after thirty seconds. FTS verification
runs its integrity command inside a rolled-back savepoint. Probes repair nothing and make no
exhaustive integrity claim; writers quarantine a corrupt index
([recovery](index-storage-contract.md#corruption-recovery)).
