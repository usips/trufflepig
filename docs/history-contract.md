# Captured history and exact historical source

## Endpoints and commands

History captures the calling worktree's HEAD when opening an operation. Traversal
membership and coverage belong to that tip; divergent worktrees never share one
repository-wide current HEAD. Immutable object/change facts may be reused after
rewrites. `hist-status` reports the captured tip, cutoff, visited count, depth
limit, traversal boundary, failure, and completion.

`hist-index` persists a captured-tip request and schedules work. `--wait` polls
that view until completion or a reported failure while the root daemon remains
available for queries; foreground mode processes bounded batches locally. `--no-daemon hist-index` performs one bounded foreground batch
unless `--wait` is supplied. `hist TARGET` lists first-parent changes from indexed
traversal; foreground mode first indexes a bounded batch. A history request
examines at most 256 indexed commits, reports `examined`, and sets truncation
when more remain. Result pagination pages the retained result set, not additional
unexamined commits. Commit metadata is indexed; file changes are computed on
demand and cached, as stated by `changes: computed_on_demand`.

`since REV [TARGET]` compares the exact resolved revision directly with captured
HEAD. It never substitutes a merge base. `--uncommitted` reconciles and compares
to one published live generation with a capture interval and per-file revisions.
That endpoint includes eligible untracked files and is not an atomic filesystem
snapshot. Deletions require published observations; ignored/excluded or unknown
coverage remains distinct. Before entries identify Git blobs; after entries
identify published live source and can become stale when disk changes.

`diff REV [--target TARGET]` compares a commit with its first parent, including a
root commit's empty preimage. Unscoped output contains metadata summaries.
Explicit targets permit budgeted hunks with separate before/after byte coordinates
and one context line for file changes. Oversized hunks may be omitted while their
persisted handles remain readable. Invalid UTF-8 hunks and `show` reads use
exact byte-escaped text with an explicit encoding label.

`blame TARGET` returns contiguous attribution runs with first-parent traversal,
movement detection (`-M`), and ignored whitespace (`-w`). `--raw` disables those
last two options. `--ignore-revs-file FILE` supplies explicit exclusions; no
formatter revisions are inferred. Live handles use their published source buffer
with a captured-HEAD race guard; historical handles use their stored commit
endpoint. Supplied-buffer reads disable configured clean/process filters and
reject other attribute conversions that change the verified bytes. Attribution
identifies commits, not an agent's ownership of edits.
Blame output can truncate to the response budget.

## Selection and correspondence

Targets accept root-relative `path:`, exact `sym:`, and persisted source handles.
Symbol selection uses the current live index. Duplicate declarations remain
separate; ambiguous symbol names return selectable candidates. Selected live
occurrence coordinates and revisions must match the historical postimage before
symbol correspondence can follow backward. Stale or uncertain correspondence
fails explicitly instead of silently choosing a same-name declaration. A live
symbol selector against an older `diff` revision can therefore return
`stale_source`; a path selector supports arbitrary historical revisions.

The shared source differ uses pinned `imara-diff` histogram matching over
original-byte line tokens. Declaration correspondence uses unique declaration
keys and line correspondence; exact-content moves require an unambiguous match.
Failed/incomplete extraction cannot prove deletion. Whitespace differences remain
changes; they are not classified as semantically harmless.

Unique exact-blob file renames can be followed. Uncertain renames remain
additions/deletions. Subtree roots map paths through their repository prefix;
reads cannot leave that scope, and correspondence stops at a scope boundary.
Historical tree entries and exact reads preserve percent-encoded arbitrary Unix
filename bytes. Subtree root prefixes and blame target/ignore-file paths currently
require UTF-8.

## Immutable reads

Change entries retain repository common-directory identity, commit, blob, path,
BLAKE3 content revision, and half-open original-byte span independently on each
side. `show HANDLE --side
before|after` requires an explicit side, verifies tree membership and exact blob
identity, and renders those bytes. Continuations preserve that identity and the
remaining range across edits and restarts, subject to the shared ten-minute
result-cache expiry and eviction policy in the [retrieval contract](retrieval-contract.md).
Historical entries cannot be passed to live `ctx`.

Git remains the source archive. Objects removed by garbage collection produce an
explicit unavailable error. The history database does not archive source against
Git object loss. SHA-1 and SHA-256 object identities and linked worktrees are
supported; replacement refs and grafts are explicitly unsupported.

## Git access and background lifecycle

History requires native Git 2.55 or newer. Unavailable Git disables historical
commands without disabling ordinary search. One subprocess abstraction passes
argument arrays and literal paths, suppresses lazy fetching, optional writes,
external diff/textconv, pagers, hooks, credential helpers, and network protocols.
Each subprocess has a fifteen-second timeout and its own process group for
cancellation. Git configuration is not changed on disk.

History storage defaults beneath the normal cache base, keyed by canonical Git
common directory. Separate clones remain separate. `--cache` isolates history
beneath that override; `--history-cache` selects an explicit shared base. One
leased worker owns each canonical history cache/common-directory pair.

Root daemons register and heartbeat independently of foreground work. The worker
polls tips every two seconds and treats ref watches as hints. Registrations
expire after fifteen seconds; no active roots means the worker exits. Startup
launches it without waiting for history indexing. `--no-daemon` launches no
background processes. Interrupted batches resume from committed checkpoints.

## Bounds and limitations

Each view captures a two-year cutoff of 730 days and at most 20,000 visited
first-parent commits. Dates filter eligibility; an old timestamp alone does not
stop traversal. Batches publish at most 128 commits atomically inside the history
database. Obsolete traversal views expire after thirty days. Publication is not
atomic across live, history, and diagnostic databases.

Historical blobs and supplied Git input are capped at 2 MiB; each subprocess has
an 8 MiB combined stdout/stderr cap. Retained history staging has a conservative
64 MiB partition: two trees at 6 MiB each, comparison indexes at 4 MiB, changes
at 8 MiB, cache serialization at 8 MiB, entries at 16 MiB, hunk rendering at
8 MiB, and Git output at 8 MiB. Charges include owned strings and conservative
container overhead; phases release their preceding buffers. Cached rows are
size-checked before loading and decoded incrementally against the change budget.
Encoded paths are bounded at 128 KiB. Working-tree file metadata uses the tree
budget and its sorted path references are bounded at 1 MiB.
Generated entries also have a 10,000-entry cap; hunk construction bounds combined
input at 100,000 newline tokens. Staging exhaustion reports a resource limit or
explicit truncation. These are retained-payload bounds, not a measured whole-process
RSS ceiling; parser, inference, and allocator internals are separate.
History SQLite has a 2 GiB page ceiling. A 256 MiB WAL ceiling reserves room for
bounded publication and requires checkpointing. Capacity checks evict reusable
change-cache entries before indexed progress stops. Blocked checkpoint/capacity
reports resource-limited coverage. These bounds do not establish foreground
latency or whole-process memory guarantees under repository-scale load.

Roots, shallow boundaries, missing local objects, symlinks/gitlinks, and resource
exclusions remain distinguishable from complete source coverage. All-ref
traversal, `when`, ghosts, cochange/hotness, historical ranking/embeddings,
transcript adapters, and paid paired-agent trials are outside this release.
