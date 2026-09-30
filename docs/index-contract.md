# Authoritative index and updates

## Storage and identity

SQLite WAL with FTS5 (`<cache>/index.sqlite3`) holds source revisions,
definitions, occurrences, relationships, and lexical documents. Immutable result
sets live in a separate `<cache>/results.sqlite3` (`src/results/result_set_store.rs`),
so saving a query's results never waits behind a publication. Source occurrence
identity includes root, path, revision, and span; content identity is separate
and supports reuse without merging distinct definitions or overrides.

Ordinary indexes retain exact identifiers. Separate FTS columns contain expanded
snake/camel identifiers, body text, and path terms. All eligible file content is
covered by bounded source regions, preferably on syntax boundaries with the
containing symbol attached. File records expose resource exclusions.
[FTS5 reference](https://www.sqlite.org/fts5.html)

## File ranking

Ordinary queries collapse each retrieval lane to its best region per file and
fuse lanes by weighted reciprocal rank; the
[ranking contract](ranking-contract.md) defines lanes, weights, evidence tiers
and the path prior. `sym:` and `re:` retain occurrences. Pages maximize file
references before adding optional symbol names.

## Coherent publication

1. Reconcile the working tree, including untracked eligible files. Filesystem
   metadata and Git status are hints; content hashing establishes revision.
2. Read eligible files into bounded buffers and reuse unchanged extraction by
   content revision, grammar extension, and extraction version. One extraction
   worker stages the full current file set on disk; cancelled extraction is
   retried. Resource bounds and failures remain visible.
3. Build the staged symbol universe and recompute relationship resolution,
   including references in unchanged callers whose targets changed.
4. Publish source revisions, definitions, occurrences, relationships, and FTS
   documents in one transaction, then advance the generation.
5. Roll back every published component if the transaction fails. Prune
   superseded content without discarding unexpired handle revision metadata.

Each published generation records a capture interval and per-file revisions.
It is a reconciled observation, not an atomic filesystem snapshot. Publication
observations distinguish edits, additions, deletions, and coverage changes;
consumers receive an explicit incomplete window after observation retention
limits are exceeded. The observation journal retains at most 256 generation
windows, 4,096 observations, and 4 MiB of observation payloads. Sessions persist
their own baseline fingerprints outside the replaceable live index; see [diagnostics](diagnostics-contract.md).

An exclusive writer lease covers staging through publication. The next lease
holder removes abandoned staging directories from interrupted indexing.

Parsing cancellation retains lexical coverage and a parse-failure status. It
publishes no structural facts represented as freshly parsed. The parser is reset
before a different input. Partially recovered facts from a completed parse are
separately labeled and cannot strengthen resolver certainty.

Only committed generations are queryable. A reader holds one snapshot through
candidate retrieval and graph joins. Readers are query-only
(`src/store/read_access.rs:open_read`): they open the index read-only, run no
schema or metadata writes, wait at most 2 s on a lock, and are interrupted when
the request's 20 s query deadline (`src/daemon/deadline.rs`) expires, answering
`timed_out`; in-request reconciliation (`--no-daemon`, an unpublished index)
pauses that clock. A missing index or generation 0 answers `index_warming`. Only
indexing, sessions, and the reconciler open the writer (`Store::open`). A reader
may read a linked worktree's current bytes through its parent member's index
(root-relative paths address the same files); its result sets are saved in the
worktree's own cache, and `show` verifies those bytes against indexed revisions.
Only a worktree sharing the member's Git common directory reads this way. Files
that may differ are worktree changes against the parent's `HEAD` (committed,
staged, unstaged, untracked) plus the parent's uncommitted and untracked files;
each Git probe is bounded to 2 s and a result is reused for 5 s
(`src/workspace/home_index/worktree_divergence.rs`). A hit's file also differs
when its worktree bytes no longer hash to the indexed revision; one query
hashes each file once and at most 128 files, and files beyond that fall back to
the divergence (`home_index/hit_verification.rs`). Hits read from current bytes
(`re:`, re-extraction) never differ; re-read files count as differing. A
definition in such a file is re-extracted from worktree bytes (up to 64 files
per `sym:` answer, which also adds definitions found only in divergent files;
`show sym:` does the same whenever any file diverges, so it agrees with
search); when they cannot be extracted (binary, or
failed extraction), `show` serves the parent index's stored bytes with
`verified: false` and `source: parent_index`. File changes between
extraction and publication may leave an indexed revision behind disk; `show`'s buffer check
prevents applying it to current bytes. Reconciliation eventually catches up.

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
(`src/store/seed.rs`): the seed copies content-addressed extraction facts and
embeddings but never a publication, so the first reconcile publishes generation 1
for the worktree root. Until then, workspace queries answer from the member's
index ([workspace](workspace-contract.md#retrieval-and-output)). Seeding writes
nothing to stdout or stderr; an attempt that does work is recorded in the
worktree cache's `seed-outcome.json`, which `ws status` reports. A linked worktree's `.git` file is never indexed as
source. A per-root daemon exits on its own when its root
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
directory (`src/system.rs:dir`). Clients treat connect errors `NotFound`,
`ConnectionRefused`, and `PermissionDenied` as "no daemon" and fall back
(`src/daemon.rs:unreachable`), first to the router's file spool
(`src/daemon/spool.rs`, directory `src/system.rs:spool_dir`), which carries the
same JSON request and reply bodies through atomically renamed files and is served
from the router's idle tick. The frame protocol and its limits apply unchanged
per hop.

The daemon reconciles at startup, then every five minutes while a watcher is
active or every 30 seconds without one. Watch events are hints that accelerate
reconciliation; events only inside `.git`, `target`, `node_modules`, or
`.trufflepig` are ignored, and overflow, watch exhaustion, and missed events
trigger recovery. Watching daemons run at nice 10 with idle I/O priority. Events use a 75 ms quiet period with a one-second maximum debounce; no
sub-debounce visibility promise applies. The recursive watcher covers the root
and filters cache events, while the indexing walk prunes ignored/build
directories. Ignored trees may still consume operating-system watches.

Reconciliation observes ignore rules and relevant configuration/include bytes;
module candidate resolution runs against the staged files. Grammar or extraction
changes require bumping the stored extraction version to invalidate cached facts.
The initial resolver may recompute the full reference universe;
there is no promise that all work is proportional to changed files.

Keep only the current graph. `refs`, `ctx`, and `map` expose structural facts;
a module map does not require PageRank. Readers do not join old result identities
to graph nodes reused by a replacement generation.

## Resource lifecycle

The authoritative SQLite page cache is 8 MiB for the writer and 2 MiB per
reader; staging uses 4 MiB. Indexing
reads at most 2 MiB per source file, retains at most 50,000 extracted facts per
file, and divides lexical coverage into regions of at most 4,096 raw bytes.
Sources containing NUL or exceeding the read cap remain file records with an
exclusion reason. General current-source reads have a separate 16 MiB cap.
Stage files live under the selected cache directory and are removed on completion.
Result and embedding retention limits are
specified in the [retrieval](retrieval-contract.md) and
[semantic](semantic-contract.md) contracts. Measure cold and warm cost before
changing storage architecture.

The socket is `daemon.sock` inside the per-root cache; a short explicit cache
path avoids Unix socket path-length limits. Requests are serialized with a
64 KiB/256-argument cap and 4 MiB reply cap. There is no protocol version or build
handshake yet; stop the daemon before replacing its binary. Automatic startup
spawns a child process and falls back to coherent local access if needed; it does
not establish detached service lifecycle guarantees.

Local `--no-daemon` searches reconcile synchronously for the live index and
launch no background process. Semantic search uses cached vectors only; an
explicit foreground `semantic prepare` acquires the root preparation lease and
performs preparation locally. Metadata/status reads do not force reconciliation.

## Bounded diagnostics probes

`doctor` checks SQLite structure, foreign keys, FTS integrity, unchanged-source
extraction identity, semantic cache provenance, and accidental model initialization.
SQL checks have a 100 ms progress deadline and a 10 ms busy wait. Extraction
samples at most four source files of at most 64 KiB, with a 250 ms inter-file
cutoff; a running extraction may finish after that cutoff. Semantic checks sample
at most sixteen vectors without loading a model.

Outcomes distinguish passed, failed, incomplete, and unavailable checks. Source
drift is reported separately from corruption; unverified cached facts are not
silently treated as valid. The daemon attempts these checks on idle ticks after
thirty seconds. FTS verification runs its integrity command inside a rolled-back
savepoint. No automatic repair or exhaustive integrity claim is made.
