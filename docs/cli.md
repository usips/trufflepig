# CLI usage

Build with `cargo build`. Commands below run the resulting Linux binary from the
repository root. `--root` defaults to the current directory; root discovery does
not walk up to Git metadata. Choose the same root and cache for follow-up reads.

```sh
cargo build
target/debug/trufflepig --root . --no-daemon index
target/debug/trufflepig --root . --no-daemon search 'refill tokens'
target/debug/trufflepig --root . --no-daemon search 'sym:TokenBucket'
target/debug/trufflepig --root . --no-daemon search 're:fn .*helper lang:rust'
target/debug/trufflepig --root . --no-daemon search 'file:src/ kind:function'
```

`search` is optional before ordinary query words. `sym:` requests an exact
case-sensitive definition name. `re:` scans live bytes. `file:` is a root-relative
path-prefix filter; `lang:` and `kind:` filter recorded classifications. Language
names are `rust`, `typescript`, `javascript`, `luau`, `dreammaker`, and `text`;
`rs`, `ts`, `js`, `lua`, and `dm` are aliases. Docs/config use `text`.

One JSON object plus newline is the default output; `--json` accepts the same
format. `-b/--budget` defaults to 600 `o200k_base` tokens for the entire serialized
stdout response. `-n/--limit` defaults to 20 candidate hits per page; the budget
may fit fewer. Increase the budget when a hit or source line cannot fit.

## Follow-up reads and navigation

Copy `hits[].handle` or `next` from the response into the appropriate command:

```text
trufflepig show SET:ORDINAL
trufflepig more SET@OFFSET
trufflepig ctx SET:ORDINAL
trufflepig refs refill_tokens
trufflepig map src/
trufflepig show path:src/main.rs:1-20
```

Use `target/debug/trufflepig` as above when the binary is not installed on PATH.
A handle is a 32-character result-set ID plus a one-based ordinal. A pagination
cursor uses the same set ID and a zero-based next offset. Handles survive restart
within their retention limits; they never name the latest unrelated query.
`stale_source` requires a fresh search or an explicitly current path read.
`stale_result` means the graph generation changed and `ctx` needs a fresh handle.

`refs` distinguishes observations, resolved targets, candidates, and unresolved
sites. `map` shows structural module facts. These are conservative navigation
features; see [language limits](language-contract.md).

Paths in responses percent-encode raw filename bytes. Paste the encoded path
unchanged into `show`, including `%20` for a space, `%25` for `%`, and `%3A` for
`:`, so filename punctuation cannot become a line-range separator. Traversal,
absolute source paths, and symlinks are rejected.

## Cache and daemon

The cache defaults to `$XDG_CACHE_HOME/trufflepig/<root-hash>`, or
`$HOME/.cache/trufflepig/<root-hash>`. `--cache DIRECTORY` overrides it. Each cache
belongs to one canonical root. Use a disk-backed cache with room for the index
and staging database; avoid RAM-backed `/tmp`.

Ordinary commands start a per-root daemon automatically. `--no-daemon` performs
local operations and synchronously reconciles before searching. Explicit `index`
reconciles locally; `status` reports existing indexed coverage. `serve` runs the
daemon in the foreground and `stop` requests shutdown. Specify the same `--root`
and `--cache` on these commands.

The daemon serializes requests, reconciles on watch events and periodically, and
has no build-version negotiation. Stop it before changing binaries. Very long
cache paths can exceed Unix socket limits; use a shorter `--cache` path. Watcher
fallback and resource limits are documented in the [index contract](index-contract.md).

## Optional CPU semantics

Default builds provide lexical/structural retrieval. Build with
`cargo build --features semantic` to enable `--sem`. Install the exact pinned
assets and runtime described in the
[CPU gate guide](../evaluation/semantic_gate/README.md), then run
`semantic-check MODEL_DIRECTORY`. Set `TRUFFLEPIG_MODEL_DIR` and `ORT_DYLIB_PATH`
for semantic queries. Missing assets are explicit `semantic_unavailable` errors;
ordinary searches do not download them.

Semantic regions are embedded synchronously on demand. Cold queries can therefore
be expensive. The [recorded CPU check](../evaluation/semantic_gate/cpu_result.json)
covers two publisher inputs and establishes no repository-level speed or quality
claim. [Evaluation usage](evaluation-contract.md) covers reproducible fixture
comparisons and the outstanding held-out/agent evaluation requirements.
