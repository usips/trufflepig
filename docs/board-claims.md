# Revisions, tasks, and events

Revision, task, claim, event, and text-bound semantics for the [board](board-contract.md).

Creation records the caller user as owner at revision 1. Proposals carry a base revision and
complete new SSOT body. Acceptance uses compare-and-swap on the plan head, creates the next
immutable revision and a decision entry atomically, and fails `stale_revision` if the base changed.
`edit` uses the same base check and marks the revision `direct`; rejection preserves the proposal
and records its decision. Decisions broadcast to participants, naming proposal and proposer. A
proposal supersedes only an open proposal on the same plan by any session of the author's user and
harness; replacement is atomic and retains both bodies. Open proposals flag a stale base.

Task ordinals are allocated atomically per plan; columns are `todo`, `doing`, `review`, `done`, and
`blocked`. Claiming sets `doing`, assignee, and scope; carve-and-claim is atomic. `--section`
follows the [shared heading-title and anchor
rules](board-web-contract.md#browser-state-and-safe-rendering). At most one unended claim exists per
task. Active competing claims fail `claim_conflict` with holder identity, model, effort, and
activity. Normal claims require scope. Claim views expose each lease's entry `E#`. `--resume`
replaces only the same user/host/harness lease across sessions and inherits omitted scope: a bare
`--resume` refreshes your own live claim in place (same entry, no new entry or event) or takes over
another session's lease once idle ten minutes, while `--resume=E#` names the current unended claim
entry and takes over immediately; takeover ends the prior lease as `resumed`. The space form
`--resume E5` is refused: `use --resume=E5`. Stale claims remain claimable. Carve retries dedupe
only while their lease remains owned; after release or takeover, another carve creates a fresh task.
Claiming a `done` task or moving it to `doing` is denied. The current claim holder may move its own
task. The actual delegator may release only a delegated claim, and any caller may release another
actor's stale claim; these release paths may move only to `todo` or `blocked`. A `release` grant has
the same limit and cannot move a delegated or stale claim to `review` or `done`. The plan owner acting
as human has all roles intrinsically; an agent needs a `decide` grant to reopen a done task as todo
and an `assign` grant to reassign a task, move an unclaimed non-done task to `todo` or `blocked` without
changing its assignee, or manage its workers. A plain move under an assign grant cannot release
another actor's active claim; an authorized `--to` reassignment can end it as reassigned. Assign
authority alone does not authorize moves into `review` or `done`; those moves require the active claim
holder. Task creation, assignment, and authorship alone grant no such authority. Moving out of `doing`
ends the claim.
Before handing work back, post progress and move the task to `todo`, unless the human requested a
pause. `review` and `done` transitions retain their existing idempotent behavior.

`task P7.3 doing` without `--to` claims the caller using prior scope or title. An assigned `doing`
card without a lease reserves nonprivileged claims and moves for its assignee. Task-addressed entry
and question delivery, read-time recipients, and Task Show projection follow the
[worker evidence contract](board-workers.md). Exact-identity feedback ownership and grant boundaries
live in the [feedback contract](board-feedback-contract.md) and
[board contract](board-contract.md#identity-references-and-authority).
Claimant writes on the plan and inbox calls refresh activity. Matching commit co-authors refresh
only current leases with `claimed_at <= committed_at <= ingest time`, using the maximum activity
timestamp. Staleness follows reloadable `claim_ttl_minutes` (120 by default); authorized takeover
names/notifies the prior holder. Do not take over a live claim without an explicit authorized
resume. Reminders include unanswered questions and identify questions left by an earlier task
holder so the current holder can respond. `show` separates active/stale claims, claimable cards, and
headings without cards; bounded Task Show detail is defined by the
[CLI contract](board-cli-contract.md#collection-bounds).

Each mutation has one global event sequence. Inbox, Attention, Overview, and Claims follow the
[read scope contract](board-projects.md#read-scopes). Own events are excluded unless they are
feedback outcomes. `scanned_through` is the highest examined sequence in one snapshot, separate
from `rendered_through`. Query-cap or render-budget truncation acknowledges only the last rendered
event; a complete fully rendered query acknowledges `scanned_through`, including irrelevant tails
and empty reads. Explicit `inbox SEQ` never advances a cursor. First inbox seeds the latest
`min(limit,20)` events in a 500-event scan window, then bounded open reminders. Reminder totals are
exact up to 200 (a lower bound beyond), and stale-base proposals appear only in their author's
reminders. Reminders never acknowledge events. Cursor acknowledgements and lease renewal create no
events. The same full actor identity shares a cursor; use explicit `SEQ` to reread.

Entry text is nonblank and at most 4096 UTF-8 bytes; SSOT/proposal text is at most 32768 bytes and
may be empty. Plan titles are nonblank and at most 256 bytes. The serialized daemon frame is capped
at 65536 bytes including JSON escaping. Oversize errors report encoded size and advise splitting the
plan. `show` and `review` default to 4000 output tokens; other board/feedback commands default to
1500. `-b/--budget` and `-n/--limit` bound the selected JSON or lines output.
