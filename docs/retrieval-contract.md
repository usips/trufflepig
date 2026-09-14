# Search and source inspection

## Retrieval

Ordinary search combines case-sensitive exact identifier lookup and
identifier-aware lexical retrieval. Original identifiers and snake/camel
expansions have separate index representations. Fixed snapshot, query, and
options determine ranking, with stable source-path and byte-span tie breaks;
wall-clock deadlines do not silently select which lanes contribute.

`re:<pattern>` searches live files. Path, language, and kind filters apply before
result limits. Tests, docs, and configuration remain eligible. Hits can identify
a symbol, a source region, or a file. Searchable source regions cover the entire
eligible file, including long-symbol middles and gaps between definitions.
Resource-excluded files remain discoverable by path with their exclusion reason.

Coverage describes indexed files, parser failures, semantic coverage, and result
truncation separately. An interrupted or incomplete search cannot claim an
exhaustive absence of hits. An empty complete search is successful; unavailable
lanes and stale source are explicit responses.

## Immutable handles and pagination

Each hit contains an immutable result-set identifier and an ordinal within that
set. `show <handle>` never resolves against a client's latest search. Concurrent
and consecutive queries cannot redirect an existing handle.

`more <cursor>` names its result set and next ordinal explicitly. Pagination
preserves original ranking and source revisions. Persisted sets survive daemon
restart until their ten-minute expiry. Expiration and eviction produce explicit
errors, never an alias to a newer set. Retention is bounded by 32 MiB total,
50 sets, and 10,000 hits per set; capped searches report truncation.

## Reads and graph freshness

`show <handle>` opens and reads the source once, hashes that buffer, compares it
with the handle's revision, and renders only that same buffer. Mismatch returns
`stale_source`; indexed coordinates never address changed bytes. Moved or deleted
sources fail explicitly. In-place concurrent writes may produce an unmatched
buffer, which also fails the revision check.

`show path:a-b` explicitly reads current coordinates without claiming indexed
freshness. Paths are root-relative. Reject absolute paths, traversal, and symlink
escapes; verify the opened file remains within the root. A path-like filename
containing separators used by the CLI is reversibly escaped rather than guessed.

Only the current graph is retained. `ctx <handle>` requires the handle's index
generation to match the current graph; otherwise return `stale_result`. The agent
obtains current graph handles with a fresh search. `refs` exposes occurrence
evidence and resolver provenance as described in the
[language contract](language-contract.md).

## Coordinates, serialization, and budgets

Half-open original-file byte spans are canonical. Displayed lines are one-based
and inclusive. Count newline bytes in the original buffer; CRLF is one break.
BOM bytes stay in offsets. Any decoding transformation carries a mapping back to
original bytes; invalid encoding never silently shifts source coordinates.

Paths use reversible escaping, including control bytes and invalid UTF-8.
Output never injects ANSI control sequences through repository text. Structured
responses preserve the same identities, spans, evidence, and coverage as text.

Every response is bounded by a named tokenizer applied to the complete serialized
response, including headers, escaping, metadata, truncation notices, and the final
newline. The default is 600 tokens; no minimum hit count is promised. A tokenizer
count is only a guarantee for that tokenizer, not an estimate guaranteed for
another model. Tiny budgets return a fitting explicit status or no bytes when
no complete status fits. Pagination has a fresh response budget.

Lifecycle fields such as creation/expiry times and random set identifiers are
excluded from the deterministic-ranking guarantee. Source and metadata are
clearly distinguishable; source excerpts do not become tool instructions.
