# CLI usage

Install the Linux binary with `cargo install --path . --locked`. Put Cargo's
binary directory (`$CARGO_HOME/bin`, default `$HOME/.cargo/bin`) on `PATH`;
`command -v trufflepig` verifies discovery.
`--root` defaults to the current directory. Without a workspace, root discovery
does not walk up to Git metadata; use the same root and cache for follow-up reads; configured workspaces route reads to their recorded member.

```sh
trufflepig --help -b 2000
trufflepig --root . --no-daemon index
trufflepig --root . --no-daemon search 'refill tokens'
trufflepig --root . search 'sym:TokenBucket'
trufflepig --root . search 're:fn .*helper lang:rust'
trufflepig --root . --no-daemon search 'file:src/ kind:function'
trufflepig --root . semantic prepare --wait
trufflepig --root . --no-daemon semantic prepare --wait
trufflepig --root . semantic status
trufflepig semantic worker status
trufflepig semantic worker stop
```

Use an explicit `search` verb for all queries; unknown commands exit 2 with an
error. No command means `status`. `sym:` selects exact, case-sensitive symbol
occurrences; `re:` scans live bytes and returns matching occurrences. `file:` is
a root-relative path-prefix filter; `lang:` and `kind:` filter recorded
classifications. Language names are `rust`, `typescript`, `javascript`, `luau`,
`dreammaker`, and `text`; `rs`, `ts`, `js`, `lua`, and `dm` are aliases. Docs/config use `text`.
One JSON object plus newline is the default output; `--json` accepts the same
format. `-b/--budget` defaults to 600 `o200k_base` tokens for the serialized
stdout response. Search ranks files first and emits one compact representative
per file before the `-n/--limit` page cap (20 files by default); the budget may
fit fewer. Each hit includes a `file` URI, line span, and immutable handle.
Increase the budget when a hit or source line cannot fit.
Budget failures include a retry hint on stderr even when no JSON error fits.
Help is also budgeted; use `--help -b 2000` for full flag descriptions.

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

An explicit `--workspace FILE` selects named local checkouts. Otherwise, discovery
checks the nearest ancestor `trufflepig.workspace.toml`, then the global registry.
`--no-workspace` retains singleton operation. An explicit subtree `--root` never
silently expands to a configured member root.
Ordinary workspace queries search all members. `in:NAME` or `--member NAME` selects
a member; `ws:home` selects the invocation checkout and `ws:all` searches the
complete workspace. File-first ranking fuses member lanes with provenance;
compact hits carry a percent-encoded `file` URI, lines, and owner-qualified
handle. Pagination is frozen to captured member snapshots, and missing members
are reported as incomplete coverage.
Use `show`, `more`, and `ctx` from any member of the same workspace; persisted
handles retain their original owner. Explicit paths, history revisions, sessions,
and other owner commands use home or `--member NAME`. `ws discover` only returns
a proposed configuration and does not scan unrelated directories. See the
[workspace contract](workspace-contract.md) for schema, cache behavior, routing
guarantees, and current limits.

## Follow-up reads and navigation

Copy `hits[].handle` or `next` into the appropriate command. Search pages use
compact file-first entries; `show` supplies source lines for a selected handle.

```text
trufflepig show SET:ORDINAL
trufflepig more SET@OFFSET
trufflepig ctx SET:ORDINAL
trufflepig refs refill_tokens
trufflepig map src/
trufflepig show path:src/main.rs:1-20
```

A handle is a 32-character result-set ID plus a one-based ordinal. A pagination
cursor uses the same set ID and a zero-based next offset. Handles survive restart
within their retention limits; they never name the latest unrelated query. In a
workspace, the result set freezes each member owner and generation, so later
ranking or configuration changes cannot retarget a handle.
`show` continuations retain source identity and remaining bytes; pass their
`next` value unchanged to `show`. `stale_source` requires a fresh search or an
explicitly current path read.
`stale_result` means the graph generation changed and `ctx` needs a fresh handle.
`refs` reports symbol occurrences and distinguishes observations, resolved
targets, candidates, and unresolved sites. `map` shows structural module facts.
These are conservative navigation features; see [language limits](language-contract.md).

Paths in responses percent-encode raw filename bytes. Paste the encoded path
unchanged into `show`, including `%20` for a space, `%25` for `%`, and `%3A` for
`:`, so filename punctuation cannot become a line-range separator. Traversal,
absolute paths, and symlinks are rejected.

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
`path:` selectors are root-relative. `sym:` uses exact live symbol occurrences;
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
overlap labels, and incomplete-window limits.

## Cache and daemon

The cache defaults to `$XDG_CACHE_HOME/trufflepig/<root-hash>`, or
`$HOME/.cache/trufflepig/<root-hash>`. `--cache DIRECTORY` overrides it. Each
cache belongs to one canonical root; use disk-backed storage for the index and
staging database and avoid RAM-backed `/tmp`. History defaults to the normal
cache base keyed by canonical Git common directory, so linked worktrees share
immutable Git facts. An explicit `--cache` isolates history; `--history-cache
DIRECTORY` selects a shared history-cache base.
In workspace mode, `--cache` supplies an isolated base containing coordinator
state and separate member caches.
Ordinary singleton commands start a per-root index daemon automatically. Workspace
queries start a coordinator automatically unless `--no-daemon` is set;
`ws show` and `ws status` inspect locally. The coordinator starts member daemons
when needed and reads their published indexes. Semantic requests may start the
shared per-user inference worker when semantic retrieval is enabled.
`--no-daemon` performs local indexing and launches or contacts no background
process, including the inference worker. Search then uses published indexes and
cached semantic vectors only. `semantic prepare --no-daemon` is the explicit
foreground preparation path and holds the root preparation lease while it runs.
Explicit `index` reconciles locally; `status` reports existing indexed coverage.
For a singleton root, `serve` runs the index daemon in the foreground and `stop`
requests shutdown. In workspace mode, `stop` stops only the coordinator; stop a
member daemon with `trufflepig --no-workspace --root ROOT stop`. Specify matching
`--root` and `--cache` values for the daemon being controlled. Daemon startup also
launches a leased history worker without awaiting indexing. `hist-index --wait`
continues history batches until completion or explicit failure.
`doctor` runs bounded integrity/provenance probes without starting inference.
The index daemon attempts probes while idle. `semantic status` reports root
preparation state; `semantic worker status` reports shared-worker residency.

The daemon serializes requests and reconciles on watch events and periodically;
it has no build-version negotiation. Stop it before changing binaries. Very long
cache paths can exceed Unix socket limits; use a shorter `--cache` path. Watcher
fallback and resource limits are documented in the [index contract](index-contract.md).

## Optional semantic retrieval

Default builds provide lexical/structural retrieval. Install with
`cargo install --path . --locked --features semantic` for CPU inference or
`--features semantic-cuda` for CUDA support. Configure the pinned model,
ONNX Runtime library, provider, GPU UUID, and ORT arena in
`$HOME/.config/trufflepig/inference.toml`; model and runtime paths may also use
`TRUFFLEPIG_MODEL_DIR` and `ORT_DYLIB_PATH`. Run `semantic-check MODEL_DIRECTORY`
to verify assets and provider execution before preparing a root.
`semantic prepare` schedules missing source-region vectors for background
preparation; `semantic prepare --wait` waits for the requested generation.
`semantic status` reports preparation coverage. `semantic worker status` and
`semantic worker stop` inspect or stop the shared per-user worker. Missing assets,
provider failures, pending vectors, and query timeouts remain explicit statuses
while lexical results stay available. `--no-sem` disables semantic retrieval,
including a workspace's persistent opt-in; `--rerank`/`--no-rerank` also toggles rerank.
Source regions are never embedded synchronously during search; search uses the
cache and has a 500 ms semantic query deadline before returning lexical fallback.
The [semantic contract](semantic-contract.md) defines worker, cache, snapshot,
and status behavior. [Evaluation usage](evaluation-contract.md) defines the
reproducible fixture and development-task measurements.
