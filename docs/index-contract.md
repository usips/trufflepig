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

## Daemon and invalidation

Each canonical root has one daemon, protected by an exclusive startup lock and
a length-prefixed JSON Unix socket protocol. Worktrees keep separate databases.
A stale socket does not permit a second writer while the startup lock is held.

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

Local `--no-daemon` searches reconcile synchronously. Metadata/status reads do
not force reconciliation. No incremental parse tree, parallel parse pool, or
background embedding queue is implemented.
