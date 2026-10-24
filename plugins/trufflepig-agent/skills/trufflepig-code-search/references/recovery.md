# Worktree fallback and recovery

Read this when a footer says `warming`, `served from`, or `differs`, when a read
reports `verified: false`, or when a search fails, is busy, or returns nothing.

## Linked worktrees

A linked worktree is its own checkout: run from it (`cd DIR && trufflepig-agent ...`
or `--root DIR`).

- Standalone parent reads use the main checkout's default cache, even with a
  custom `--cache`; that cache holds the worktree's results and divergence.
- Workspace parent reads use the configured member cache under the workspace
  base; worktree results and divergence stay separate.
- Index-based parent reads need a writable selected result cache. Explicit
  current paths read bytes without a cache; `refs` stays index-only.
- While the worktree index warms, the footer reads like
  `lunatic@wt warming → served from lunatic index (3 files differ)`. Without a
  parent it says `warming (no parent index)`.
- A `differs` hit can keep parent coordinates. `show` re-extracts changed files;
  failed re-extraction may show unverified parent bytes, so check `verified` and
  `source` before claiming current bytes.
- An exact `sym:` miss with no parent-index candidate refreshes divergence and
  checks at most 64 changed files. If more paths changed, Git probing fails, or a
  changed file stays unchecked, coverage is partial and truncated: absence is
  not exhaustive and workspace home does not widen.
- Fallback reads reuse divergence for five seconds; `show`/`ctx` handles keep
  parent identity, and `more` pages survive parent republishes.

See the [workspace contract](https://github.com/usips/trufflepig/blob/master/docs/workspace-contract.md)
and [index contract](https://github.com/usips/trufflepig/blob/master/docs/index-contract.md).

## Empty, busy, and failed calls

- For a complete empty search, refine terms or try one exact or `re:` search
  before a bounded fallback. `warming` with a `served from` footer is an answer.
- Reads share one 30 s budget and at most five attempts, including startup,
  transport, and fallback. Board/feedback writes retry typed contention at most
  twice; other writes run once. Never replay an ambiguous write automatically.
- `show FILE`, `show path:FILE`, and `read:path:` continuations use one
  current-file fallback after explicit `daemon_busy` retries, or immediately on
  pre-dispatch `index_cache_unavailable`. A busy retry reserves 250 ms for that
  fallback; other timeouts and database locks do not trigger it.
- For cache-free discovery, use `--no-cache search 'sym:NAME'`,
  `--no-cache search 'file:NAME kind:file'`, or `--no-cache map FILE`. These
  bounded current-source reads write no cache and skip semantic/rerank lanes.
- `--no-daemon search/map` also falls back on a typed pre-dispatch cache
  refusal; `cache_fallback` names the selected cache and cause. Neither mode
  bypasses source permissions or root containment.
- Current-source coverage names caps and unread files; partial coverage never
  proves absence.
- Copy returned `read:path:` handles to `show`. They bind current bytes to their
  verified root, expire when bytes change, and have no persisted `more` page.
- Use `ws:all` or `in:MEMBER` for dependency ownership. An unreadable home path
  can suggest a verified configured member without redirecting to it.
- After another busy search, retry manually at most once after load clears, then
  use the bounded fallback above. Do not loop, change cache identity, strip
  rejected flags, or restart services for repair.
- For an unreadable known path, use the harness's bounded exact-file reader and
  report the failure.

See the [CLI contract](https://github.com/usips/trufflepig/blob/master/docs/cli.md)
and [runtime contract](https://github.com/usips/trufflepig/blob/master/docs/runtime-contract.md).
