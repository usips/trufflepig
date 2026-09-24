# CLI usage

Install the Linux binary with `cargo install --path . --locked`. Put Cargo's
binary directory (`$CARGO_HOME/bin`, default `$HOME/.cargo/bin`) on `PATH`;
`command -v trufflepig` verifies discovery.
`--root` defaults to the current directory. Without a workspace, root discovery
does not walk up to Git metadata; use the same root and cache for follow-up reads; configured workspaces route reads to their recorded member.

```sh
trufflepig --help -b 2000
trufflepig --root . --no-daemon index
trufflepig --root . search 'sym:TokenBucket'
trufflepig --root . search 're:fn .*helper lang:rust'
trufflepig --root . --no-daemon search 'file:src/ kind:function'
trufflepig --root . semantic prepare --wait
trufflepig --root . semantic status
trufflepig semantic worker status
```

Use an explicit `search` verb for all queries; unknown commands exit 2 with an
error. No command means `status`. `sym:` selects exact, case-sensitive symbol
occurrences, declarations before modules, members, locals, and imports; `re:` scans live bytes and returns one occurrence per matching
line, with `^` and `$` matching at line boundaries as in grep. `file:` is
a root-relative path-prefix filter; `lang:` and `kind:` filter recorded
classifications. Language names are `rust`, `typescript`, `javascript`, `luau`,
`dreammaker`, and `text`; `rs`, `ts`, `js`, `lua`, and `dm` are aliases. Docs/config use `text`.
One JSON object plus newline is the default output; `--json` accepts the same
format. `--format lines` renders `search`, `refs`, `map`, `more`, and `show` as
tab-separated lines for agents that read output directly: one
`HANDLE<TAB>[MEMBER/]PATH:START-END[<TAB>NAME]` line per hit, each followed by
an indented `  LINE: TEXT` snippet, then a one-line `coverage:` summary, then `next: CURSOR` and `truncated: true` when present;
`show` prints a `PATH [(MEMBER)] lines FIRST-LAST` header, `LINE<TAB>text`
rows, and a `verified:` footer (`verified: current file` for explicit path reads,
which JSON marks `"source": "current_file"`) that flags `encoding: byte-escaped`
once. Errors and every other verb stay JSON. `show` defaults to a 1500-token
budget, never below a workspace's `[output].budget`; `-b/--budget` otherwise defaults to 600 `o200k_base`
tokens for the serialized stdout response, measured on the rendered text of the
selected format, or to the workspace's `[output].budget` when one is set. Search ranks files first and emits one compact representative
per file before the `-n/--limit` page cap (20 files by default); the budget may
fit fewer. Each hit includes a `file` URI, line span, and immutable handle, and
a `snippet` (`line`, `text` of at most 120 characters): the first line in the
span that mentions a query term or the hit's name, preferring code over comments.
A page keeps snippets while at least eight hits (or all remaining hits) fit,
otherwise it drops them for more locators. Snippets locate evidence; `show`
remains the verified read. Increase the budget when a hit or source line cannot fit.
Budget failures include a retry hint on stderr even when no JSON error fits.
`--help` and `--version` print plain, unbudgeted text; help ends with a query
and navigation summary.

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
silently expands to a configured member root. Unselected workspace queries search
the home member and widen to all members when home has no hits; see the
[workspace contract](workspace-contract.md#retrieval-and-output).
`in:NAME` or `--member NAME` selects a member; `ws:home` selects the invocation
checkout, which may be a linked Git worktree standing in for its member, and
`ws:all` searches the complete workspace. File-first ranking fuses
member lanes with provenance; compact hits carry a percent-encoded `file` URI,
lines, and owner-qualified handle. Pagination freezes captured member snapshots;
missing members are reported as incomplete coverage. Use `show`, `more`, and `ctx`
from any member of the same workspace; persisted handles retain their original
owner. Explicit paths, history revisions, sessions, and other owner commands use
home or `--member NAME`. `ws discover` returns only a proposed configuration and
does not scan unrelated directories. See the [workspace contract](workspace-contract.md)
for schema, cache behavior, routing guarantees, and current limits.

## Follow-up reads and navigation

Copy `hits[].handle` (the first column in lines format) or `next` into the
appropriate command. Search pages use compact file-first entries, whose `name`
appears only for symbol hits; `show` supplies source lines for a selected handle.

```text
trufflepig show SET:ORDINAL
trufflepig more SET@OFFSET
trufflepig ctx SET:ORDINAL
trufflepig refs refill_tokens
trufflepig map src/
trufflepig show path:src/main.rs:1-20
trufflepig show 'sym:refill_tokens file:src/'
```

`show 'sym:NAME'` (with optional `file:`, `lang:`, `kind:`) reads the best-ranked
definition with that exact name as a verified handle read: declarations before
modules, members, then locals and imports. The footer adds `definitions: N` and
up to five `also: PATH:START-END KIND` locators for the others; a missing name
fails with `no_definition`.

A handle is a 32-character result-set ID plus a one-based ordinal. A pagination
cursor uses the same set ID and a zero-based next offset. Handles survive restart
within their retention limits; they never name the latest unrelated query. In a
workspace, the result set freezes each member owner and generation, so later ranking
or configuration changes cannot retarget a handle. `show` continuations retain
source identity and remaining bytes; pass their `next` value unchanged to `show`.
`stale_source` requires a fresh search or an explicitly current path read.
`stale_result` means the graph generation changed and `ctx` needs a fresh handle.
`refs` reports symbol occurrences and distinguishes observations, resolved targets,
candidates, and unresolved sites. `map` shows structural module facts: modules and
types under a prefix, plus functions, methods, constants, and macros when the
prefix names exactly one file. These are
conservative navigation features; see [language limits](language-contract.md).

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

The [runtime contract](runtime-contract.md) defines cache ownership, daemon
lifecycle, systemd upgrades, and sandbox spool transport.

## Optional semantic retrieval

Default builds provide lexical/structural retrieval. Install with
`cargo install --path . --locked --features semantic` for CPU inference or
`--features semantic-cuda` for CUDA support. Configure the pinned model, ONNX Runtime
library, provider, GPU UUID, and ORT arena in `$HOME/.config/trufflepig/inference.toml`;
model and runtime paths may also use `TRUFFLEPIG_MODEL_DIR` and `ORT_DYLIB_PATH`. Run
`semantic-check MODEL_DIRECTORY` to verify assets and provider execution before
preparing a root. `semantic prepare` schedules missing source-region vectors for
background preparation; `semantic prepare --wait` waits for the requested generation.
`semantic status` reports preparation coverage. `semantic worker status` and
`semantic worker stop` inspect or stop the shared per-user worker. Missing assets,
provider failures, pending vectors, and query timeouts remain explicit statuses
while lexical results stay available. `--no-sem` disables semantic retrieval,
including a workspace's persistent opt-in; `--rerank`/`--no-rerank` also toggles
rerank. Source regions are never embedded synchronously during search; search uses
the cache and has a 500 ms semantic query deadline before returning lexical fallback.
The [semantic contract](semantic-contract.md) defines worker, cache, snapshot, and
status behavior; [evaluation usage](evaluation-contract.md) defines the reproducible
fixture and development-task measurements.
