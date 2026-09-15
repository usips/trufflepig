# Workspace contract

A workspace federates explicitly named local repository roots. Members have names
and paths, without project roles or inferred implementation ownership. An agent
interprets matches from each repository; Trufflepig exposes evidence and origin.
The [retrieval contract](retrieval-contract.md) applies within every member.

## Membership and discovery

```toml
[workspace]
name = "space"

[semantic]
enabled = true

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
at most 256 KiB.

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
selection requires the invocation directory to belong to a member. Multiple
matching configurations require explicit selection; stale entries report an
actionable configuration error. `--no-workspace` disables discovery and conflicts
with `--workspace`.

Home is the member containing the invocation directory. An explicitly selected
workspace can have no home. A member subdirectory searches the full configured
root unless an explicit `--root` requests a subtree. An explicit root must exactly
match a member to activate automatic workspace discovery; an explicit workspace
with a nonmatching explicit root is an error.

`ws show` reports configuration, home, roots, and availability. `ws status` adds
available publication generations and coverage. `ws discover PATH...` validates
the supplied directories and returns a proposed TOML configuration in JSON. It
does not apply the proposal, fetch repositories, or discover unrelated siblings.

## Retrieval and output

Ordinary queries search all members. `in:NAME` and `--member NAME` select a
member; `ws:home` selects home and `ws:all` selects the whole workspace. Selectors
do not establish dependency, import, or compiler-resolution relationships.

Each member produces its existing ranked candidate list. Retrieval collapses
each lane to one representative occurrence per file, then fuses file ranks with
stable path and span tie breaks. The coordinator combines member file lanes with
member provenance; raw scores from separate indexes are not compared. There is
no elapsed-time lane omission.

Every emitted hit includes its member name. The page's `members` mapping resolves
represented names to percent-encoded canonical roots. Coverage distinguishes
unavailable members and retained candidate counts from exhaustive match counts.
Per-member `partial` and `issues` retain extraction, live-read, and semantic
coverage limitations. Each member receives an equal share of the retained
candidate/byte ceiling; omissions are explicit rather than exhaustive counts.
Missing or not-yet-published members produce partial coverage. If no selected
member is available, the command returns an explicit unavailable outcome.

The normal 600-token `o200k_base` budget applies once to the complete JSON
response, including provenance and coverage. Compact entries carry a
percent-encoded `file` URI, line span, and owner-qualified handle. Labels cannot
be removed to squeeze in additional hits. Tiny budgets retain explicit
insufficient-budget behavior. Pages capture each member's publication generation
separately and freeze those owners for follow-up reads; they are not atomic
snapshots spanning repositories.

## Immutable navigation

Workspace results persist independently of member result caches. `SET:ORDINAL`
and `SET@OFFSET` retain the existing ten-minute expiry and result-cache limits.
Each persisted entry captures its owning root, filesystem identity, cache,
generation, source revision, and original-byte coordinates. Pagination preserves
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

Each canonical root retains its index daemon and publication lifecycle. The
workspace coordinator demand-starts a member daemon when none is running and
reads published member databases without starting competing index writers. An
existing member daemon is not asked to reconcile immediately; member freshness
comes from its root watcher and periodic reconciliation. Unavailable members
remain visible in coverage. `--no-daemon` performs federation and member
indexing in the foreground and launches or contacts no background process,
including the shared inference worker. An explicit foreground semantic
preparation command holds the root preparation lease.

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
