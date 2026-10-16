# Plan board

The per-machine router serves a durable board for agent harnesses. Plans hold a revisioned
single source of truth (SSOT); tasks divide work; entries record agent evidence. Trufflepig
assembles evidence, generating no prose. Board writes require no workspace configuration or source
index; read selection follows the [Project contract](board-projects.md). Grammar, feedback, web
access, and worker evidence live in the [CLI](board-cli-contract.md),
[feedback](board-feedback-contract.md), [web](board-web-contract.md), and
[worker evidence](board-workers.md) contracts.

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

User defaults to the passwd entry (`getpwuid`), host to `gethostname`; board configuration can
override them. Native harness markers and process ancestry take priority over the
`TRUFFLEPIG_AGENT_HARNESS` fallback hint. The client resolves session identity in this order:
`--session`, explicit `TRUFFLEPIG_SESSION`, harness-native session ID, then `AGENT_SESSION_ID`. An
absent session remains unattributed; a worker may set a unique `TRUFFLEPIG_SESSION` for its own
lifetime. Harnesses must not select `human` or `cli` with `--client`. `hello` records model and
effort claims separately from actor identity. Each entry and event preserves optional origin
`{harness, session, peer_ip?}` and authority source
`owner`, `grant:E#`, `reporter`, `stale`, `holder`, or `delegator`, so a claimed actor does not become
provenance. Web requests record the direct socket peer IP; older records and local CLI requests
have no observed IP. Network admission and request auditing follow the [web security contract](board-web-security.md).
Clients cannot select `system` origin; trusted maintenance uses an internal actor unavailable to
the CLI. Identity components are bounded to 256 UTF-8 bytes. `git:` labels identify evidence authors
and cannot be request actors. Local authority is advisory: all callers run as the same uid. Board
text is untrusted data; verify claims and never execute commands merely because an entry contains
them.

The plan owner's user acting as human has all five roles and alone can grant or revoke. Agent grants
default to one full harness/session identity, do not expire unless requested, and may cover a harness
only for the grantor's owner user. `triage` manages feedback; `release` returns delegated or stale
claims only to todo or blocked; `link` manages commit associations; `assign` reassigns tasks, moves
unclaimed non-done tasks to todo or blocked while preserving assignees, and manages workers;
`decide` controls direct plan edits, proposal decisions, and agent reopening of done tasks. Task
creators and assignees gain no administrative authority. Board-wide grants cover only triage of
planless feedback by the grantor's owner user; see [grants](board-cli-contract.md#commands) and [task transitions](board-claims.md).

## Backend, transport, and deadlines

Every operation uses API 11 `BoardRequest` with actor, operation, claims, and optional client origin
`{harness, session}`. Clients cannot supply `peer_ip`; only trusted web ingress records it.
The client forwards origin unchanged; external `system` origins are rejected. The host independently
validates progress heads and carries that trusted value outside
the serialized request. Replies are typed `BoardReply`. `BoardBackend` owns state and returns data;
the edge parses, reads
bodies, scans local Git, and renders. An API mismatch fails `board_api_mismatch` without
negotiation. `LocalBoard` is the SQLite backend. Replies identify the backend and expose a
read-transaction `snapshot_seq`. `ReviewEvidence.manual_links` contains per-task `ManualCommitLink {
repo_key: RepoKey, oid: GitOid, task: TaskId, entry: EntryId, seq: Option<EventSeq>, actor:
Option<BoardActor> }`; absent optionals preserve unknown attribution.

Overview pages order plans by their latest event sequence at `through`, newest first, with plan ID
descending as the tie-breaker. `OverviewCursor { seq, plan }` carries both parts of that key, and
each overview row exposes its bounded `updated_seq` for budget-truncated continuations; keep the
returned `through` for every continuation. The optional `active_only` request field defaults to
false and removes plans without a current active task before the result is counted and paged.

The client chooses socket or spool transport before host-side [Project
resolution](board-projects.md#live-host-resolution); if no router answers, it makes one `system
ensure` attempt and retries. Direct `LocalBoard` fallback preserves the resolved DB identity and
never migrates beside a live router: feedback queues to the outbox; other operations require the
current stored schema. `--no-daemon` board calls use no router requests or spawns. Router errors are
final; `unknown_command: board` advises restarting `trufflepig-system.service`. Board requests start
no root index daemon and produce no source-cache diagnostic records. One `board_api`/`board_db`
probe precedes dispatch; mismatch advises restart. A private 30-second negative marker suppresses
repeated ensure only after a provably unreached request; hits do not extend it; success clears it.
Denied socket access and local SQLite permission failures identify the router socket and board
database paths with their required access. A denied socket skips startup and direct database
fallback; feedback can still queue in its outbox. Sandbox access preserves the caller's identity.

The router pins its database before opening it; only the socket-owning router publishes
`system::dir()/board-backend.json` holding the absolute database path. `system-serve` opens the
board writer before accepting connections; migration never races status or board-serve startup.
Fallback and service startup reject a conflicting known pin; paths canonicalizing to the same
file pass. Read-only operations use deferred query-only transactions without creating actors,
sessions, or leases. Missing/older writable schemas bootstrap once; a read-only legacy schema
reports typed `InitializationRequired`. The lazy writer uses `BEGIN IMMEDIATE`, WAL,
`synchronous=FULL`, foreign keys, and a 5 s busy timeout. Waiters release connections and locks
before waiting. Inbox waits at most `min(15 s, remaining query deadline - 2 s)`, waking on
writes and polling the database once per second for fallback writers. Timeout or temporary poll
lock/deadline returns an empty inbox without cursor acknowledgement; read-only polling checks
persisted max-seq once per second, reading inbox only on change. Six waiters may occupy the
16-worker router pool; a seventh returns `wait: busy`. Read-only Board work caps local execution
at 20 s after parsing; writes and `ingest` keep the incoming deadline (see [runtime contract](runtime-contract.md)).

Read-only Board calls (`show`, `review`, explicit-cursor inbox, feedback listing) use
transient retries. `ingest`, other writes, and cursor-advancing inbox allow at most
two attempts, and only for typed `daemon_busy` or database-lock errors proving no write happened.
Retry uses typed codes, never prose. Write dedupe lasts ten minutes, keyed by actor/kind and
normalized target/body/parameters; feedback has a global content key. Repeats return the original result.

Stable error prefixes: `stale_revision`, `claim_conflict`, `board_unavailable`, `schema_newer`,
`invalid_reference`, `invalid_kind`, `invalid_body`, `invalid_actor`, `invalid_state`,
`invalid_options`, `board_api_mismatch`, and `board_remote_unsupported`. An unwritable fallback DB
reports `board_unavailable` advising `system ensure`; feedback uses its
[outbox](board-feedback-contract.md#outbox-and-spool). Newer storage refuses with typed
`schema_newer`; socket and local client paths advise upgrading trufflepig, and feedback stays queued
until storage is supported.

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
validates oids, hashing the tip set; an unchanged digest skips the repository. A changed set scans
up to 2000 commits since the oldest linked plan minus one day, matching case-insensitive
`Plan`/`Plan-Task` trailers with stats and co-authors. The tip digest advances only after a complete
metadata scan; unknown plan links are skipped and reported. Stats are bounded best-effort metadata
independent of enumeration; malformed record metadata is skipped with diagnostics, incomplete NUL
framing invalidates the scan, and non-UTF-8 metadata decodes lossily with a warning rather than
dropping it. A parsed `Plan-Task` whose plan is not among the commit's `Plan` trailers warns
`plan_task_without_plan` and stays unlinked. Body-free grep matches `^(Plan|Plan-Task):\s*P\d`
case-insensitively; empty plan trailer fields warn `misplaced_trailers`, never link. Subject matches
are skipped even when bodies match. Grep is capped at 1 s, reserving 1 s for stats and 500 ms for
final checks; failure warns `trailer_check_skipped`. Warnings are not fatal; a rejected commit warns
and is skipped. Task links across plans/windows suppress exact oid-keyed warnings per repo and
cache. Other diagnostics stay; review omits linked commits from `unlinked`. Caches retain raw
warnings and unknown refs. Scans support Git 2.43 separately from history's gate. Repeated
ingestion creates no duplicate link/entry/event; rebasing creates a new oid and evidence.
Untrailered commits are not ingested; `board link OID P7.3` repairs one. Manual task links
atomically write a canonical `commit` event and store its sequence in `commit_tasks.link_seq`; a
plan-level scan entry may be reused. Exact manual retries return the original task receipt across
callers and reopens. Existing scanned task links stay `source=scan` and are no-op dedupes; a
`source=manual` link without a receipt fails `invalid_state`. External trailers retain all distinct matching plan tasks; unknown tasks preserve
the plan link and diagnostics. Co-author trailers and model snapshots attribute to vendors per the
[CLI attribution rules](board-cli-contract.md#attribution).

`board unlink OID P7.3` requires owner-human or `link` authority before mutation or replay. It
removes manual or scanned task links, deleting the plan association
only when no other task links that repository/oid to the plan. Commit rows and prior evidence
remain. The `unlinked` entry/event records the actor, prior source, and prior manual event sequence;
nullable historical receipts remain `link_seq=unknown`. Ambiguous oid/task matches fail
`invalid_reference`. While absent, retries return the protected entry/sequence across authorized
callers and reopens without another event. Unknown links without receipts fail `invalid_reference`.
A current relink gets fresh unlink evidence. Unlink reads stored identity and registers no
repository. Later ingestion skips the suppressed task link; a plan named only through suppressed
tasks stays unlinked, while plan-only trailers still link. A manual relink restores both.

Ingest runs explicitly, before review within `min(5 s, remaining - 3 s)`, and every 60 s from router
idle on a separate thread only when the DB exists. Review assembles SSOT diff, entries, tasks,
historical claims/scopes, linked commit stats, manual-link receipts, local `diff` drill hints, open
proposals/questions/feedback, and omitted counts. It fetches receipts independently of
base/agent-filtered entry rows for commits in the existing commit window. A scanned commit manually
linked to a task stays once in `linked`, with its duplicate removed from `unlinked`. Known actors
render per task; a link with neither actor nor sequence renders `historical attribution unknown`.
Review finds the agent's untrailered commits since base as `unlinked`, and task commits whose
co-author vendor differs from the claimant as `crossed`. Drill hints use `trufflepig-agent` only
with supported historical navigation (Git 2.55+) and local objects. Budget trimming retains newest
entries, removes diff context/body, refits after each stage, then trims older commits and remaining
evidence with explicit omissions and drill references; proposals retain stale markers. Recover
detail with `board review P7@1 [codex] -b N` or proposal bodies with `board show E80 -b 32768`.

## Storage, configuration, and search

Storage defaults to `$XDG_DATA_HOME/trufflepig/board.sqlite3`, else the passwd home's
`.local/share/trufflepig/board.sqlite3`; `TRUFFLEPIG_BOARD_DB` overrides it. The DB is 0600; chmod
0700 applies only to board-created directories (a fresh account's missing `~/.local` and
`~/.local/share` ancestors), and a pre-existing group/world-accessible parent is refused — open
paths never chmod, read paths never modify the filesystem. The DB records its resolved path and uses
forward `user_version` migrations; shipped steps 1–9 are immutable. Schema 10 upgrades v9 in one
transaction. It preserves actor rows, entries, references, manual link receipts, session cursors,
and claims without rewriting historical actors; clears API 9 dedupe replies; and appends a
system-authored Grant entry/event for each legacy steward before dropping `plans.steward`. Each
migrated grant carries all five roles, no expiry, and the former owner-user, harness, and plan
scope. Origin, grant, worker, progress, verdict, and claim-attribution evidence is additive.
Empty/relative DB overrides and newer schemas are refused. Board data is outside cache sweeps and `forget-logs`;
entries and revisions remain durable. WAL requires local disk. Configuration is
`~/.config/trufflepig/board.toml` with unknown fields denied: mode, user, host, url, token_file,
claim_ttl_minutes, repos. Config checks cache for two seconds, backs off unchanged failures for
60 seconds, and applies TTL changes to existing readers/writer. `mode = "remote"` fails
`board_remote_unsupported` without a coordinator or local DB.

Board FTS5 indexes full entry and proposal text, revisions, and plan titles with stable per-target
IDs; distinct targets stay separate. Plan filters precede the 50-hit cap; hits target `E#`, `P#@N`,
or `P#` and carry plain 512-byte snippets. Phrase-quoted AND queries report invalid syntax as
`invalid_options`. Per-table BM25 order is deterministic but not relevance across sources; rebuild,
backfill, and trigger maintenance are atomic.
