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
result limits, and every lane and live walk shares one path matcher
([`PathFilter`](../src/search/path_filter.rs)). Values are percent-encoded like
indexed paths (`file:café/` and `file:caf%C3%A9/` agree). A bare `%` is encoded as
a literal percent; valid `%HH` escapes are accepted case-insensitively, so use
`%25` when a literal percent precedes two hex digits. A `file:` value matches the
root-relative paths it prefixes when some indexed path continues it at `/`,
`.`, or its end; otherwise it matches where it starts a path component
(`file:script/host` matches `crates/a/src/script/host.rs`, never `ghost.rs`, and
`file:src` beside only `src-tauri/` also reaches `crates/a/src/`). Repeated
`file:` values OR together; `-file:` values exclude paths they match at any
component start. `map` lists a file's members when its prefix names exactly one
indexed file (`map ./src/x.rs`, `map host.rs`), never for a directory prefix.
An empty `lang:` filters nothing; otherwise
`lang:` names a recorded language, case-insensitively: `rust`, `typescript`,
`javascript`, `csharp`, `php`, `luau`, `dreammaker`, or `text`. `rs`, `ts`/`tsx`,
`js`/`jsx`, `cs`, `c#`, `phtml`, `lua`, and `dm` are aliases; `md`, `markdown`,
`toml`, `json`, and `txt` mean `text`, which covers docs, configuration, and
every other extension.
`.php` and `.phtml` files are `php`. Other names fail with `unknown_language`.
Tests, docs, and configuration remain eligible; free-text ranking demotes tests
and docs unless a query targets them, favors phrase matches, and ranks an
identifier's definitions and uses first (see
[file ranking](ranking-contract.md)). Hits can identify
a symbol, a source region, or a file. Searchable source regions cover the entire
eligible file, including long-symbol middles and gaps between definitions.
Resource-excluded files remain discoverable by path with their exclusion reason.

Coverage describes indexed files, parser failures, semantic coverage, and result
truncation separately. An interrupted or incomplete search cannot claim an
exhaustive absence of hits. An empty complete search is successful; unavailable
lanes and stale source are explicit responses. An empty filtered search adds
`filter_diagnosis` to coverage: how many indexed paths each `file:` value
matched, with up to three nearest paths by name when it matched none,
`lang:X matched 0 files`, and whether the filters only fail together or
`-file:` excluded every match. Workspace lines pages name the members sharing
each explanation.

## Immutable handles and pagination

Persisted entries are tagged `live_source`, `commit`, or `change`. Each entry
contains an immutable result-set identifier and an ordinal within that set.
`show <handle>` never resolves against a client's latest search. Concurrent and
consecutive queries cannot redirect an existing handle.

`more <cursor>` names its result set and next ordinal explicitly; JSON pages
carry the bare cursor as `next`. Pagination
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
freshness. Paths are relative to the selected root (the selected workspace member
when a workspace is active). Reject absolute paths, traversal, and symlink escapes;
verify the opened file remains within the root. If a missing path exists relative to
the invocation subdirectory, report the equivalent root-relative spelling. A
path-like filename containing separators used by the CLI is reversibly escaped
rather than guessed.

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
`--format lines` selects the tab-separated renderer for result pages and `show`
(`src/results/lines.rs`, `src/source/lines.rs`); it carries the same handles,
paths, line spans, cursors, and truncation flags with a one-line coverage summary,
keeps byte-escaped text verbatim, and never applies to errors or other verbs.

Every stdout response is bounded by `o200k_base` applied to the complete serialized
response in the selected format, including headers, escaping, metadata, truncation
notices, and the final newline. The default is 600 tokens; no minimum hit count is promised. A tokenizer
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
