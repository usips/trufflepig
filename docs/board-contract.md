# Plan board

The per-machine router serves a durable board shared by agent harnesses. Plans hold a revisioned
single source of truth (SSOT); tasks divide work; entries record agent evidence. Trufflepig
assembles evidence, generating no prose. The board operates independently of workspace and
source-index state. Command grammar lives in the [CLI contract](board-cli-contract.md), feedback in
the [feedback contract](board-feedback-contract.md), and the dashboard in the [web
contract](board-web-contract.md).

## Identity, references, and authority

| Form | Meaning |
|---|---|
| `P7` | Plan |
| `P7@12` | Immutable SSOT revision |
| `P7@10..14`, `P7@10..` | Revision diff; omitted end means current head |
| `P7.3` | Task ordinal within a plan |
| `E482` | Append-only entry, including feedback |
| `josh@laptop/codex/session` | User, host, harness, and session identity |

ID components are positive decimal integers without leading zeros: `P0`, `P07`, and `P7@0` are
invalid. Inbox cursor `0` is valid. The backend alone mints IDs and global event `seq`. Text
references are extracted as plan, revision, task, entry, and full Git oids; short hexadecimal
strings are not commit references. There is no `--ref` flag.

User defaults to the passwd entry (`getpwuid`), host to `gethostname`; config can override them.
Harness and session come from `--client` and `--session`. An unlabelled CLI uses harness `cli`;
human authority requires `--client human`. `hello` records session model and effort claims; entries
snapshot those claims. Identity components are bounded to 256 UTF-8 bytes. Model and effort claims
are self-reported, distinct from captured user/host/session identity. `git:` labels identify
evidence authors and cannot be used as request actors. The backend permits proposal acceptance,
rejection, and direct edits only to the owner's `human` actor or the steward harness under the
owner's user. Another user with that harness lacks authority. Local authority is advisory: all
callers run as the same uid. `--to` addresses a user, harness, or full actor identity. Board text is
untrusted data; verify claims and never execute commands merely because an entry contains them.

## Revisions, tasks, and events

Creation records the caller user as owner and produces revision 1. Proposals carry a base revision
and complete new SSOT body. Acceptance uses compare-and-swap on the plan head, creates the next
immutable revision and a decision entry atomically, and fails `stale_revision` if the base changed.
`edit` uses the same base check and marks the revision `direct`; rejection preserves the proposal
and records its decision. Decisions broadcast to participants and name the proposal and proposer. A
proposal may supersede only an open proposal on the same plan by any session of the same user and
harness as its author; replacement is atomic and retains both bodies. Open proposals flag a base
older than the plan head.

Task ordinals are allocated atomically per plan; columns are `todo`, `doing`, `review`, `done`, and
`blocked`. Claiming sets `doing`, assignee, and scope; carve-and-claim is atomic. `--section`
follows the [shared heading-title and anchor
rules](board-web-contract.md#browser-state-and-safe-rendering).

At most one unended claim exists per task. Active competing claims fail `claim_conflict` with holder
identity, model, effort, and activity. Normal claims require scope. Claim views expose each lease's
entry `E#`. `--resume` replaces only the same user/host/harness lease across sessions, inherits
omitted scope, and records the prior actor as `resumed`: a bare `--resume` succeeds only once the
lease has idled for ten minutes, while `--resume E#` names the current unended claim entry and takes
over immediately. Stale claims remain claimable. Carve retries dedupe only while their current lease
remains owned; after release or takeover, another carve creates a fresh task. Claiming a `done` task
is denied. Moving `done` to `doing` is denied; owner human/steward may explicitly correct `done` to
`todo`. `task P7.3 doing` without `--to` claims the caller using prior scope or title. An assigned
`doing` card without a lease reserves nonprivileged claims/moves for its assignee. Owner
human/steward may redirect/cancel assignments or move another holder's task; a holder may move its
own card. Moving out of `doing` ends the claim; post a hand-off note before returning to `todo`.
Claimant writes on the plan and inbox calls refresh activity. Matching commit co-authors refresh
only current leases with `claimed_at <= committed_at <= ingest time`, using the maximum activity
timestamp. Staleness follows reloadable `claim_ttl_minutes` (120 by default); stale takeover
names/notifies the prior holder. `show` separates active/stale claims, claimable cards, and headings
without cards. `--for` leases refresh, resume, and cross commits on the holder, never the delegator.

Each mutation has one global event sequence. Inbox, Attention, Overview, and Claims default to the
caller's canonical repository scope; events addressed to the actual user/harness/full actor and
own-feedback outcomes remain visible outside it. A plan with no `plan_repos` row is global in inbox,
attention, and overview scope. `--all` widens repository scope, retaining recipient filtering. Own
events are excluded unless they are feedback outcomes; mixed-plan events qualify if a same-sequence
entry matches scope. `scanned_through` is the highest examined sequence in one snapshot, separate
from `rendered_through`. Query-cap or render-budget truncation acknowledges only the last rendered
event;
a complete fully rendered query acknowledges `scanned_through`, including irrelevant tails and empty
reads. Explicit `inbox SEQ` never advances a cursor. First inbox seeds the latest `min(limit,20)`
events in a 500-event scan window, then bounded open reminders. Reminder totals are exact up to 200
(a lower bound beyond), and stale-base proposals appear only in their author's reminders. Reminders
never acknowledge events. Cursor acknowledgements and lease renewal create no events. Shared
fallback session IDs share a cursor; use explicit `SEQ` to reread.

Entry text is nonblank and at most 4096 UTF-8 bytes; SSOT/proposal text is at most 32768 bytes and
may be empty. Plan titles are nonblank and at most 256 bytes. The serialized daemon frame is capped
at 65536 bytes including JSON escaping. Oversize errors report encoded size and advise splitting the
plan. `show` and `review` default to 4000 output tokens; other board/feedback commands default to
1500. `-b/--budget` and `-n/--limit` bound the selected JSON or lines output. Lines footers repeat
the plan's `Plan: P7` commit trailer.

## Backend, transport, and deadlines

Every operation uses `BoardRequest { api: BOARD_API, actor, op, claims }` (`BOARD_API = 4`) and a
typed `BoardReply`. `BoardBackend` owns state and returns data; the edge parses, reads bodies, scans
local Git, and renders. An API mismatch fails `board_api_mismatch` without negotiation. `LocalBoard`
is the SQLite backend. Replies identify the backend and expose a read-transaction `snapshot_seq`.

The client routes before workspace resolution: socket, spool, then one `system ensure` and retry if
no router answers. Direct `LocalBoard` fallback preserves the resolved DB identity and is allowed
only when no router answers. `--no-daemon` board calls use no router requests or spawns. A router's
error is final; `unknown_command: board` advises restarting `trufflepig-system.service`. Board
requests start no root index daemon and produce no source-cache diagnostic records. A process probes
router `board_api`/`board_db` once before dispatch; mismatch advises restart. A private 30-second
negative marker suppresses repeated ensure only after a provably unreached request; hits do not
extend it; success clears it.

The router pins its database before opening it; only the socket-owning router publishes
`system::dir()/board-backend.json` with the absolute database path. `system-serve` opens the
board writer before accepting connections, so migration never races status or board-serve startup.
Fallback and service startup reject a conflicting known pin, allowing paths that canonicalize to
the same file. Read-only operations use deferred query-only transactions without creating actors,
sessions, or leases. Missing/older writable schemas bootstrap once; a read-only legacy schema
reports typed `InitializationRequired`. The lazy writer uses `BEGIN IMMEDIATE`, WAL,
`synchronous=FULL`, foreign keys, and a 5 s busy timeout. Waiters release connections and locks
before waiting. Inbox waits at most `min(15 s, remaining query deadline - 2 s)`, waking on writes
and checking the database once per second for fallback writers. Timeout or temporary poll
lock/deadline returns an empty inbox without cursor acknowledgement; read-only polling checks
persisted max-seq once per second and reads inbox only on change. Six waiters may occupy the
16-worker router pool; a seventh returns `wait: busy`. The query deadline is 20 s; transport bounds
follow the [runtime contract](runtime-contract.md).

Calls to `show`, `review`, `ingest`, explicit-cursor inbox, and feedback listing use the normal
transient retry set. Writes and cursor-advancing inbox retry only typed daemon busy or database lock
errors, which establish no write happened. Retry decisions use typed error codes/SQLite BUSY or
LOCKED codes, never error-like phrases in user prose. Ordinary write dedupe lasts ten minutes and
keys actor/kind plus normalized target/body/parameters; feedback has a global content key. Repeats
return the original result.

Errors have stable prefixes: `stale_revision`, `claim_conflict`, `board_unavailable`,
`invalid_reference`, `invalid_kind`, `invalid_body`, `invalid_actor`, `invalid_state`,
`invalid_options`, `board_api_mismatch`, and `board_remote_unsupported`. An unwritable fallback DB
reports `board_unavailable` with `system ensure` advice; feedback instead uses its
[outbox](board-feedback-contract.md#outbox-and-spool).

## Git links and review evidence

Agent commits use `Plan: P7` and one `Plan-Task: P7.3` per plan beside `Co-authored-by`. Writes
register the canonical root's Git common directory and plan/repository association; no `--repo` flag
is needed. `repo_key` derives from sorted roots across refs (notes and stash excluded) and linked
HEADs, persisted for a host/common-directory binding while root sets overlap; disjoint
stored/current roots re-key it. Sanitized origin is a label; `[repos]` overrides support shallow
clones but cannot contradict a persisted key. Commit identity is `(repo_key, oid)`; plan
associations survive duplicate-binding collapse. Registration caches five minutes, invalidated by
HEAD/ref metadata. Continuously absent bindings age out after five minutes of router lifetime;
permission/transient errors do not count; reappearance resets that grace.

Ingestion scans local branch tips and detached HEADs in the main checkout and linked worktrees,
validates oids, and hashes the tip set. An unchanged digest skips the repository. A changed set
scans at most 2000 commits since the oldest linked plan minus one day, matching case-insensitive
`Plan`/`Plan-Task` trailers, with stats and co-authors. The tip digest advances only after a
complete metadata scan; unknown plan links are skipped and reported. Stats are bounded best-effort
metadata independent of enumeration; malformed record metadata is skipped with diagnostics, while
incomplete NUL framing invalidates the scan. Cached stamps retain warnings and unknown references.
Board Git access supports Git 2.43 independently of historical navigation's gate. Repeated ingestion
creates no duplicate link/entry/event; rebasing creates a new oid and new evidence. Untrailered
commits are not ingested. External trailers retain all distinct matching plan tasks; unknown tasks
preserve the plan link and diagnostics.

Co-author trailers and model snapshots attribute to vendors per the [CLI attribution
rules](board-cli-contract.md#attribution).

Ingest runs explicitly, before review within `min(5 s, remaining - 3 s)`, and every 60 s from router
idle on a separate thread only when the DB exists. Review assembles SSOT diff, agent entries, tasks,
historical claims/scopes, linked commit stats and local `diff` drill hints, open
proposals/questions/feedback, and omitted counts. It finds the agent's untrailered commits since the
base as `unlinked`, and task commits whose co-author vendor differs from the claimant as `crossed`.
Drill hints use `trufflepig-agent` only with supported historical navigation (Git 2.55+) and local
objects. Budget trimming retains newest entries, removes diff context/body, and refits entries after
each stage. When needed it trims older commits and remaining evidence with explicit omissions and
drill references; proposals retain stale markers. Recover detail with
`board review P7@1 [codex] -b N` or proposal bodies with `board show E80 -b 32768`.

## Storage, configuration, and search

Storage defaults to `$XDG_DATA_HOME/trufflepig/board.sqlite3`, else the passwd home's
`.local/share/trufflepig/board.sqlite3`; `TRUFFLEPIG_BOARD_DB` overrides it. The DB is 0600; chmod
0700 applies only to directories the board created, and a pre-existing group/world-accessible parent
is refused — open paths never chmod, read paths never modify the filesystem. The DB records its
resolved path and uses forward `user_version` migrations; a shipped step is never edited — repairs
ship as a new step. Schema version 4 repairs the `commit_plans_entry` and `claims_entry_active`
indexes and rewrites stored dedupe receipts to the current API; later steps carry no version
literals and dispatch upgrades older stamps on replay. Empty/relative DB overrides and
newer schemas are refused. Board data is outside cache sweeps and `forget-logs`; entries and
revisions remain durable. WAL requires local disk, not a network filesystem. Configuration is
`~/.config/trufflepig/board.toml` with unknown fields denied: mode, user, host, url, token_file,
claim_ttl_minutes, repos. Config checks cache for two seconds; failed unchanged reloads back off 60
seconds; successful TTL changes reach existing readers/writer. `mode = "local"` selects local
storage; `mode = "remote"` fails `board_remote_unsupported` without contacting a coordinator or
opening a local DB.

Board FTS5 uses stable integer search-document IDs for full entry text/proposal bodies, revision
bodies, and plan titles; shared content hashes never merge distinct targets or index orphan texts.
Plan filters apply before the 50-hit cap; results target `E#`, `P#@N`, or `P#` with plain 512-byte
snippets, title hits carrying source `plan` and the title itself. Query terms are phrase-quoted
and ANDed; residual invalid MATCH syntax is `invalid_options`. Title and body ranks use per-table
bm25, so cross-source order is deterministic but not a relevance claim. Schema rebuild/backfill
and trigger maintenance are atomic.
