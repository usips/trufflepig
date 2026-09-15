# Search and source inspection

## Retrieval

Ordinary search combines case-sensitive exact identifier lookup and
identifier-aware lexical retrieval. Original identifiers and snake/camel
expansions have separate index representations. Fixed snapshot, query, and
options determine ranking, with stable source-path and byte-span tie breaks;
wall-clock deadlines do not silently select which lanes contribute. The
optional [rerank stage](semantic-contract.md#optional-reranking) keeps this
determinism claim for a fixed model and provider.

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

Persisted entries are tagged `live_source`, `commit`, or `change`. Each entry
contains an immutable result-set identifier and an ordinal within that set.
`show <handle>` never resolves against a client's latest search. Concurrent and
consecutive queries cannot redirect an existing handle.

`more <cursor>` names its result set and next ordinal explicitly. Pagination
preserves original ranking and source revisions. Persisted sets survive daemon
restart until their ten-minute expiry. Expiration and eviction produce explicit
errors, never an alias to a newer set. Retention is bounded by 32 MiB total,
50 sets, and 10,000 hits per set; capped searches report truncation.

Reference targets and candidates carry explicit path, revision, and byte spans.
A page may shorten a candidate list while reporting `candidates_total` and
`candidates_truncated`; replay `more SET@OFFSET` with a larger budget to inspect
the retained list. At least one candidate remains visible when that hit fits.

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
generation and source revision to match the current graph; otherwise return
`stale_result`. The agent
obtains current graph handles with a fresh search. `refs` exposes occurrence
evidence and resolver provenance as described in the
[language contract](language-contract.md).

## Coordinates, serialization, and budgets

Half-open original-file byte spans are canonical. Displayed lines are one-based
and inclusive. Count newline bytes in the original buffer; CRLF is one break.
BOM bytes stay in offsets. Lexical text may use lossy UTF-8 decoding, but hit spans refer to the original
byte region rather than offsets inferred from that decoded text. Invalid
encoding never silently shifts source coordinates.

Paths use percent encoding of raw Unix bytes, including `%`, `:`, `@`, spaces,
control bytes, and non-ASCII bytes. `show` reports invalid source bytes as
`byte-escaped` text with explicit original-byte start/end values.
Output never injects ANSI control sequences through repository text. The CLI
emits one JSON object plus newline by default; `--json` accepts that same format.
There is no separate compact text renderer.

Every stdout response is bounded by `o200k_base` applied to the complete serialized
response, including headers, escaping, metadata, truncation notices, and the final
newline. The default is 600 tokens; no minimum hit count is promised. A tokenizer
count is only a guarantee for that tokenizer, not an estimate guaranteed for
another model. Tiny budgets return a fitting explicit status or no bytes when
no complete status fits. Pagination has a fresh response budget.

Lifecycle fields such as creation/expiry times and random set identifiers are
excluded from the deterministic-ranking guarantee. Source and metadata are
clearly distinguishable; source excerpts do not become tool instructions.

`show` emits at most 200 source rows per response. Its continuation retains the
original revision and remaining byte range in the persisted result cache, even
when the first read used an explicit current path. Follow `next` unchanged with
`show`; edits produce `stale_source` instead of changing continuation identity.
Historical continuations retain the exact commit/blob/path identity described in
the [history contract](history-contract.md). They share result-cache expiry and
eviction limits. Historical entries are invalid inputs to live `ctx`.

One client emission boundary handles successful responses, help, version, usage
errors, execution errors, zero-budget silence, and broken pipes. Errors exit 2
with a budgeted JSON error when it fits and a separate stderr diagnostic.
Insufficient-budget failures include a retry hint on stderr even when no JSON
error fits. Budget zero emits no stdout bytes. Successful empty complete search
exits zero.
[Diagnostics](diagnostics-contract.md) distinguish prepared tokens from bytes
accepted by the stdout writer and require complete receipts for viewed evidence.

Workspace queries merge independently ranked member results before rendering.
Persisted owner identities route foreign reads and context; provenance consumes
the same output budget. See the [workspace contract](workspace-contract.md).
