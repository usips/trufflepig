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
2. Read changed source into bounded buffers, extract with bounded workers, and
   stage results on disk. Resource bounds and failures remain visible.
3. Build the staged symbol universe and recompute relationship resolution,
   including references in unchanged callers whose targets changed.
4. Publish source revisions, definitions, occurrences, relationships, and FTS
   documents in one transaction, then advance the generation.
5. Roll back every published component if the transaction fails. Prune
   superseded content without discarding unexpired handle revision metadata.

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
a versioned Unix socket protocol. Worktrees keep separate databases. A stale
socket does not permit a second writer while the startup lock is held.

The daemon reconciles at startup and periodically. Watch events are hints that
accelerate reconciliation; overflow, watch exhaustion, and missed events trigger
recovery. Event coalescing has no unsupported sub-debounce visibility promise.
The process excludes its own index, build output, and ignored directories from
watching where possible.

Ignore rules, relevant configuration, include order, extraction grammar/query
versions, and resolver configuration invalidate their dependent facts. The
initial correctness-oriented resolver may recompute the full reference universe;
there is no promise that all work is proportional to changed files.

Keep only the current graph. `refs`, `ctx`, and `map` expose structural facts;
a module map does not require PageRank. Readers do not join old result identities
to graph nodes reused by a replacement generation.

## Resource lifecycle

Bound database page caches, source buffers, parser concurrency, result cache,
and inference concurrency. Stage files under the configured disk-backed scratch
root and remove completed staging. Result and embedding retention limits are
specified in the [retrieval](retrieval-contract.md) and
[semantic](semantic-contract.md) contracts. Measure cold and warm cost before
changing storage architecture.
