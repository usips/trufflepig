# Board workers and evidence

Worker registration, task evidence, and commit attribution for the
[plan board](board-contract.md). Command grammar lives in the
[CLI contract](board-cli-contract.md); task state and handoff rules live in
[board claims](board-claims.md).

## Worker registration and binding

Register a worker before spawning it. The current task-claim holder, the actual delegator, or an
`assign` grantee may add or end workers; task authorship and assignment alone grant no authority.
Each registration belongs to the current claim and records its task-lifetime-unique handle, role,
model, effort, scope, and files. Roles are `code`, `review`, `test`, and `research`. Handles contain
1–32 lowercase ASCII letters, digits, `_`, or `-`.

Each registration accepts 1–64 distinct file paths. Repeat `--files PATH` for additional paths;
comma-separated entries are also accepted and trimmed. Empty or whitespace-only paths, paths over
1,024 bytes, and paths containing control characters are invalid; duplicate paths are rejected.
When one worker's scope covers more than 64 paths, divide ownership into coherent, preferably
nonoverlapping groups and register a distinct handle for each group before spawning. Match each
worker's scope and brief to its file group. The command syntax is in the
[CLI contract](board-cli-contract.md#commands).

The worker calls `board hello MODEL [EFFORT] --worker P7.3/HANDLE` in its own session. Binding
requires the registered model and effort, matching user and host, and one active actor per worker
and worker per actor. If native session identity is unavailable, set a unique `TRUFFLEPIG_SESSION`
for that worker's lifetime. End a worker with `done`, `failed`, or `stopped`; ending its claim closes
all its active workers as `claim_ended`. `board bye` ends the caller's session, releases its claims
to `todo`, and stops only its own non-done worker node.

## Task reads and addressed posts

Task Show returns the current claim, worker records, and their progress evidence; it includes at most
20 claim-history records, 50 task entries, and 50 commits, with omitted counts. It also returns
open-question count, oldest question and its captured time, latest commit time, a fixed `through`,
server time, and claim TTL. A task post stores the explicit task reference even when its text omits
it. Task recipients resolve to the current unended claim holder and bound workers when read, never
to a frozen recipient list. The owner human web view sees all open questions even when they target a
task. A plan-level question without `--to` remains broadcast; `--to human` targets only the plan
owner's human actor, not every agent session under that user. Question reminders on takeover follow
the [claim contract](board-claims.md).

## Structured progress and review verdicts

A progress post may include a full commit oid with `--head`, a `K/N` step, `PASSED/FAILED` test
counts, repeatable `--gate NAME=pass|fail|running` values, and a positive `--eta` duration capped
at 365 days and stored as an absolute backend timestamp. Each field is optional and persists
independently for its claim and optional worker;
the newest value for each field wins and records its own source entry and update time. Gates replace
by name. Gate names are nonblank, 1–64 UTF-8 bytes, and contain no controls, NUL, or `=`. The host
verifies that a supplied full commit oid exists in a repository linked to the plan; this
proves the object exists, not that tests or gates passed. Readouts show field age from each field's
own timestamp.

A review post may include exactly one verdict: `clean`, `findings=N`, or `blocking=N`, where `N` is
a nonnegative count. The verdict is structured evidence alongside the review text.

## Commit attribution

The scanner records commits as internal `system/ingest` activity and emits a distinct event sequence
for each linked commit. It retains every imported coauthor. A task link can infer a claim association
only when exactly one nonambiguous claim interval contains the commit time and at least one imported
coauthor vendor is compatible. The interval is `claimed_at < committed_at < ended_at`; an open
claim ends at ingest time. Future commits, equal interval boundaries, overlapping claims, missing
coauthors, or a vendor mismatch remain unassigned. The `claim_window` basis identifies the
responsible lead actor for evidence, not the proven writer.

`Plan-Worker: P7.3/HANDLE` may add a worker reference independently of inferred claim links. An
unknown handle is retained and warns; a known handle resolves only to that task's worker on its
compatible claim interval. Manual commit links remain durable receipts. Historical actors and
coauthor snapshots are never rewritten.
