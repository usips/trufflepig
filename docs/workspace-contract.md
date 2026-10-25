# Workspace contract

A workspace federates explicitly named local repository roots with named members
and paths; registered workspaces define [board Projects](board-projects.md#registry).
Trufflepig exposes matches and origin without inferring implementation ownership.
The [retrieval contract](retrieval-contract.md) applies within every member.

## Membership and discovery

```toml
[workspace]
name = "space"

[semantic]
enabled = true
rerank = true

[output]
budget = 1200

[members.lunatic]
path = "~/Source/lunatic"

[members.tales-from-space]
path = "~/Source/tales-from-space"

[members.tgstation]
path = "~/Source/tgstation"
```

`[workspace].members` is optional. When present, it lists every declared member
name exactly once. It does not create hidden or excluded membership. Workspace
and member names contain 1–64 ASCII letters, digits, underscores, or hyphens.
Unknown fields are errors. A configuration contains 1–32 members and occupies
at most 256 KiB. `[output].budget` sets the workspace's default response
budget in `o200k_base` tokens (1 to 1,000,000); an explicit `-b/--budget`
on the request always wins.

Member paths resolve relative to the configuration file. `~` expands to the
current user's home. Existing roots are canonical directories; missing roots
remain normalized absolute paths and are reported unavailable. Duplicate and
overlapping roots within one workspace are errors, including symlink aliases.
Separate workspace configurations may share roots.

Selection precedence is `--workspace FILE`, nearest ancestor
`trufflepig.workspace.toml`, then the global registry. The registry is
`$XDG_CONFIG_HOME/trufflepig/workspaces.toml`, falling back to
`$HOME/.config/trufflepig/workspaces.toml`:

```toml
workspaces = ["~/Source/space/trufflepig.workspace.toml"]
```

Registry paths resolve relative to the registry file. Automatic registry
selection requires the invocation directory to belong to a member: inside its
configured root, or inside a linked Git worktree of it at any location. Multiple
matching configurations require explicit selection; stale entries report an
actionable configuration error. `--no-workspace` disables discovery and conflicts
with `--workspace`.

Home is the member containing the invocation directory. An explicitly selected
workspace can have no home. A member subdirectory searches the full configured
root unless an explicit `--root` requests a subtree. An explicit root must exactly
match a member root, or a linked worktree of one, to activate automatic workspace
discovery; an explicit workspace with a nonmatching explicit root is an error.

A linked Git worktree of a member is home for that member wherever it lives,
including under the member's own tree. Worktree membership is decided by
canonical Git common directory (`src/workspace/member_root.rs`), never by path
prefix alone, so a nested unrelated repository stays part of its enclosing
member. The worktree stands in for the member's configured root: its files are
searched and read under the member's name with a separate index cache (seeded
from the member's), `ws:home` selects it, other members keep their configured
roots, and coverage reports the substitution as `member@worktree` in lines and
as `root` plus `worktree` fields in JSON. Hits carry the plain member name.

`ws show` reports configuration, home, roots, and availability. `ws status` adds
available publication generations, coverage, and a worktree's `seed` outcome.
`ws discover PATH...` validates the supplied directories and returns a proposed
TOML configuration in JSON. It does not apply the proposal, fetch repositories,
or discover unrelated siblings.

## Retrieval and output

Unscoped member queries search home. An unpublished linked-worktree home can answer from the member's
published index (`src/workspace/home_index.rs`). Workspace parent reads use the configured member cache
under the workspace base; worktree results and divergence remain separate. Standalone worktree parent
reads can use a Git-verified index from the main checkout's default cache, even when `--cache` selects
worktree results and divergence. Root mismatches fail closed; without a parent, daemon reads wait up to 8 s.
Only complete empty home results widen; warming, unavailable, or incomplete homes do not. Pages record scope; lines append it to coverage.
Outside members, queries search all members.
`in:NAME` and `--member NAME` select a member; `ws:home` selects home and `ws:all` selects the workspace.
Explicit selectors never widen or establish dependency, import, or compiler-resolution relationships.

Parent answers read worktree bytes. Coverage records `state: parent_fallback`, `home_state`, `served_from`, `differs` (`null` when unknown), and `differing_hits`. Lines read `MEMBER@WT warming → served from MEMBER index`
with `(N files differ)`, `(1 file differs)`, `(no files differ)`, or `(differences unknown)`. Changed-file hits
([index](index-contract.md)) end in `differs` until read from current bytes. Plain search, `sym:`, `map FILE`, and `show` re-extract changed files
(`served_from: MEMBER index; re-extracted in worktree`). Without a parent, home reads `MEMBER@WT warming (no parent index)`. A plain or `sym:` miss without a parent candidate refreshes divergence and examines up to
64 changed files. More paths, a failed Git probe, or an unchecked file makes coverage partial and truncated;
absence is not exhaustive and home does not widen. Other reads may reuse divergence for five seconds.
Fallback handles pin the parent publication; `show`/`ctx` enforce it, while `more` keeps pages across republishes and rejects replaced worktrees. Explicit current-path `show path:` and `read:path:`
continuations bypass selected-cache creation, marker, and seed; they work while the index warms or is
unavailable. Index-based reads preflight the selected writable result cache before parent lookup;
one at-most-2 s member slice covers cache registration, marker/seed, open, and indexing
within the original query deadline. Skipped seeding does not block a usable published parent.
`search`, `map`, `refs`, and `sym:` require an index; `refs` is index-only.

Each member produces its existing ranked candidate list. Retrieval collapses
each lane to one representative occurrence per file, then fuses file ranks with
stable path and span tie breaks. The coordinator combines member file lanes with
member provenance; raw scores from separate indexes are not compared. There is no elapsed-time lane omission.
Member ranks interleave round-robin; an opt-in rerank then reorders the top 32 merged hits with one
cross-encoder request, whose scores compare across members ([semantic contract](semantic-contract.md#optional-reranking)).

Every emitted hit includes its member name. The page's `members` mapping
resolves represented names to percent-encoded canonical roots. Coverage
distinguishes unavailable members and retained candidate counts from exhaustive
match counts. A member is `partial` only when files could not be examined,
counted as `unsearched` by kind (`walk_error`, `read_error`, `fact_limit`,
`worktree_reextraction`, `current_source_recheck`); only fact-limit paths have
the five-path sample in `issues.unsearched_paths`. Exclusions, parse failures,
and semantic/rerank status remain in `issues`. Lines read `MEMBER complete` or `MEMBER partial (N unsearched: KIND)`
and omit lanes never configured. Each member receives an equal share of the
retained candidate/byte ceiling; omissions are explicit rather than exhaustive
counts. Unpublished members report `warming`; unavailable or timed-out members retain a 120-character `reason`. No answers produce `workspace_unavailable` or `timed_out`; details name the member, phase, optional worktree root, and `show path:FILE` fallback with `absence=not_proven`.

The normal 600-token `o200k_base` budget applies once to the complete response, including provenance and coverage. Compact entries carry a percent-encoded `file` URI, line span, and owner-qualified handle; lines prefix paths with the member and name every member's `partial` and `truncated` state (`src/workspace/result_cache/coverage_lines.rs`). Labels cannot be removed to fit more hits; tiny budgets retain explicit insufficient-budget behavior. Pages freeze each member's publication generation for follow-up reads, not one atomic cross-repository snapshot.

`--no-cache search` and `map FILE` inspect current bytes in selected members without cache access; normal member and root-relative path selection applies.
The renderer round-robins member ranks under one budget and preserves every result or failure; partial scans or unavailable members cannot prove absence.
Semantics and reranking are unavailable, `refs` remains index-only, and revision-bound `read:path:` handles resolve only against configured roots; `show` rejects edits.

## Immutable navigation

Workspace results persist independently of member result caches. `SET:ORDINAL`
and `SET@OFFSET` retain the existing ten-minute expiry and result-cache limits.
Each persisted entry captures its owning root (the worktree when one stands in
for the member), filesystem identity, cache, generation, source revision, and
original-byte coordinates. A worktree owner reopens only while that worktree is
still a linked worktree of its member; a parent-index owner (`parent_index`)
reopens that same view even after the worktree publishes. Pagination preserves
the captured file-first order across repositories even when later searches or
configuration changes produce different ranks.

`show`, continuations, and `ctx` route to the stored owner from any member of the
same workspace. Configuration edits never retarget handles. Removed members and
replaced checkouts return explicit unavailable errors; edited live source retains
the normal stale-source behavior. `ctx` uses the owner's graph generation and
rejects historical entries. Historical reads verify their exact Git objects.

Explicit path reads and revision-based history commands use home or `--member`.
Without either, they require a member selection. One revision is never resolved
independently across all repositories. `refs` and `map` aggregate member-local
facts with provenance; cross-repository caller and import edges are not inferred.

## Runtime, diagnostics, and limits

Each canonical root, including a linked worktree standing in for a member,
retains its index daemon and publication lifecycle. The workspace coordinator
demand-starts a member daemon when none is running and
reads published member databases without starting competing index writers. An
existing member daemon is not asked to reconcile immediately; member freshness
comes from its root watcher and periodic reconciliation. Unavailable members
remain visible in coverage. `--no-daemon` federates in the foreground with
bounded per-member maintenance ([index contract](index-contract.md#coherent-publication));
it launches or contacts no background process, including the shared inference
worker. An explicit foreground semantic preparation command holds the root lease.

Coordinator state lives under the normal cache base. `--cache DIRECTORY` selects
an isolated workspace base containing coordinator state and separate member
caches; the override base must be outside all member roots. Singleton cache
semantics remain root-scoped. A workspace's identity is
derived from its canonical configuration path, so moving that file selects a
different result-cache namespace.

Optional semantics uses the shared per-user inference worker for one query
embedding and per-root background source preparation. Workspace retrieval reads
source vectors from member caches only; it never embeds candidate regions during
search. A 500 ms semantic query deadline returns lexical and structural member
results with an explicit semantic status when the worker or cache is unavailable.
`--no-sem` overrides the workspace's `[semantic] enabled = true` setting. Ordinary
workspace search does not initialize inference unless semantic retrieval is
enabled.

Diagnostics retain workspace/member identity and actual member retrieval timing
and rank. Surfaced evidence is counted only at the final client emission boundary.
Sessions and their baseline limits remain root-scoped; foreign evidence retains
its identity without inventing cross-repository authorship or edit ownership.

Vendor indexing, automatic dependency discovery, cross-member graph resolution,
fork/overlay matching, relationship-based ranking, and workspace-wide session
baselines are outside this contract. The example
[space workspace](../evaluation/workspaces/space.toml) exercises cross-language
navigation without a role catalog. Retrieval and navigation measurements do not
establish agent effectiveness or correct feature placement.
