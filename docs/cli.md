# CLI usage

Build with `cargo build`. Commands below run the resulting Linux binary from the
repository root. `--root` defaults to the current directory. Without a workspace,
root discovery does not walk up to Git metadata; choose the same root and cache
for follow-up reads. Configured workspaces route reads to their recorded member.

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

## Workspace search

```sh
trufflepig --workspace evaluation/workspaces/space.toml ws show
trufflepig --workspace evaluation/workspaces/space.toml search 'airlock'
trufflepig search 'crayon in:tgstation'
trufflepig search 'airlock ws:home'
trufflepig --member tales-from-space hist path:README.md
trufflepig ws status
trufflepig ws discover ~/Source/lunatic ~/Source/tales-from-space ~/Source/tgstation
```

An explicit `--workspace FILE` selects a named set of local checkouts. Otherwise,
discovery checks the nearest ancestor `trufflepig.workspace.toml`, then the global
registry. `--no-workspace` retains singleton operation. An explicit subtree
`--root` never silently expands to a configured member root.

Ordinary workspace queries search all members. `in:NAME` or `--member NAME`
selects a member; `ws:home` selects the checkout containing the invocation
directory, and `ws:all` searches the complete workspace. Results interleave member
ranks, home first and then alphabetically, with provenance inside the same token
budget. Missing members are reported as incomplete coverage.

Use `show`, `more`, and `ctx` from any member of the same workspace; persisted
handles retain their original owner. Explicit paths, history revisions, sessions,
and other owner commands use home or `--member NAME`. `ws discover` only returns
a proposed configuration; it does not apply it or scan unrelated directories.
See the [workspace contract](workspace-contract.md) for the configuration schema,
cache behavior, routing guarantees, and current limits.

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
`show` continuations retain source identity and remaining bytes; pass their
`next` value unchanged to `show`. `stale_source` requires a fresh search or an
explicitly current path read.
`stale_result` means the graph generation changed and `ctx` needs a fresh handle.

`refs` distinguishes observations, resolved targets, candidates, and unresolved
sites. `map` shows structural module facts. These are conservative navigation
features; see [language limits](language-contract.md).

Paths in responses percent-encode raw filename bytes. Paste the encoded path
unchanged into `show`, including `%20` for a space, `%25` for `%`, and `%3A` for
`:`, so filename punctuation cannot become a line-range separator. Traversal,
absolute source paths, and symlinks are rejected.

## Historical navigation

```text
trufflepig hist-index --wait
trufflepig hist-status
trufflepig hist path:src/main.rs
trufflepig hist sym:TokenBucket
trufflepig since HEAD~3 path:src/main.rs
trufflepig since HEAD --uncommitted
trufflepig diff HEAD --target path:src/main.rs
trufflepig show SET:ORDINAL --side before
trufflepig blame path:src/main.rs
trufflepig blame path:src/main.rs --raw
trufflepig blame path:src/main.rs --ignore-revs-file .git-blame-ignore-revs
```

History requires Git 2.55 or newer and local objects. Ordinary search remains
available when history is unavailable. `hist` uses captured first-parent history;
`since` compares the exact resolved revision directly with captured HEAD, or with
one published working-tree generation under `--uncommitted`. `diff` compares a
commit with its first parent; only explicit targets permit budgeted source hunks.
Historical changes require `show HANDLE --side before|after` for exact source.

`path:` selectors are root-relative. `sym:` uses exact live symbol names;
ambiguous matches return selectable handles. The [history contract](history-contract.md)
defines correspondence limits, immutable reads, resource exclusions, and coverage.

## Diagnostics and sessions

```text
trufflepig session start
trufflepig --session SESSION_ID search 'refill tokens'
trufflepig --session SESSION_ID show SET:ORDINAL
trufflepig session end SESSION_ID
trufflepig audit SESSION_ID
trufflepig forget-logs
```

Pass `--session ID` on each assigned request, or set `TRUFFLEPIG_SESSION` in the
client environment. A session is not inferred from a process or daemon. Optional
`--client NAME` labels the caller. Diagnostics default to bounded metadata;
`--diagnostics detailed` additionally records bounded raw search queries, and
`--diagnostics off` disables request recording. `audit` reports retained summaries.
See [diagnostics](diagnostics-contract.md) for delivery evidence, retention,
overlap labels, and incomplete-window limitations.

## Cache and daemon

The cache defaults to `$XDG_CACHE_HOME/trufflepig/<root-hash>`, or
`$HOME/.cache/trufflepig/<root-hash>`. `--cache DIRECTORY` overrides it. Each cache
belongs to one canonical root. Use a disk-backed cache with room for the index
and staging database; avoid RAM-backed `/tmp`. History defaults to the normal
cache base keyed by canonical Git common directory, so linked worktrees share
immutable Git facts. An explicit `--cache` isolates history beneath that override;
`--history-cache DIRECTORY` selects a shared history-cache base instead.
In workspace mode, `--cache` instead supplies an isolated base containing
coordinator state and separate member caches.

Ordinary commands start a per-root daemon automatically. `--no-daemon` performs
local operations and synchronously reconciles before searching. Explicit `index`
reconciles locally; `status` reports existing indexed coverage. `serve` runs the
daemon in the foreground and `stop` requests shutdown. Specify the same `--root`
and `--cache` on these commands. Daemon startup also launches a leased history
worker without awaiting indexing. `--no-daemon` launches no background processes;
its historical commands perform bounded foreground work. `hist-index --wait`
continues history batches until completion or an explicit failure.

`doctor` runs bounded integrity/provenance probes without starting inference.
The daemon also attempts probes and semantic residency maintenance while idle.

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
