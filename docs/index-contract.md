# Authoritative index and updates

## Storage and identity

SQLite WAL with FTS5 holds source revisions, definitions, occurrences,
relationships, lexical documents, and immutable result sets. Source occurrence
identity includes root, path, revision, and span; content identity is separate
and supports reuse without merging distinct definitions or overrides.

Ordinary indexes retain exact identifiers. Separate FTS columns contain expanded
snake/camel identifiers, body text, and path terms. All eligible file content is
covered by bounded source regions, preferably on syntax boundaries with the
containing symbol attached. File records expose resource exclusions.
[FTS5 reference](https://www.sqlite.org/fts5.html)

## File ranking

Ordinary queries collapse each retrieval lane to its best region per file
before a 1,000-file lane cap. Exact, lexical, and filename ranks combine with
reciprocal rank fusion (`k = 60`), with stable path ties. Filename evidence has
weight 2 for an exact normalized stem, 1 for all query tokens in the basename,
and 0.5 for partial path matches. Available semantic file ranks then fuse with
the combined source ranking at equal weight. `sym:` and `re:` retain occurrences.
Pages maximize file references before adding optional symbol names.

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
candidate retrieval and graph joins. File changes between extraction and
publication may leave an indexed revision behind disk; `show`'s buffer check
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
a length-prefixed JSON Unix socket protocol. Worktrees keep separate databases.
A stale socket does not permit a second writer while the startup lock is held.
Requests carry client-created UUIDs and explicit session/client context.
Semantic inference uses a separate per-user worker and lease; root daemons do
not own model sessions.
The [history worker](history-contract.md) shares immutable Git facts across
linked worktrees through a separate database, keyed by canonical common directory.
Live and history publication have independent transaction boundaries.

The daemon reconciles at startup and every 30 seconds. Watch events are hints that
accelerate reconciliation; overflow, watch exhaustion, and missed events trigger
recovery. Events use a 75 ms quiet period with a one-second maximum debounce; no
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

The authoritative SQLite page cache is 8 MiB and staging uses 4 MiB. Indexing
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
