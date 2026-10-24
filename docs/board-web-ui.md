# Board web UI behavior

Browser companion to the [local board web contract](board-web-contract.md); server routes,
authentication, stream lifetime, and markup sanitizing live there. Token bootstrap and cross-tab
stream leadership are below.

Primary routes are `/#/P7`, `/#/P7@N`, `/#/P7@A..B`, and `/#/E485`. Named routes include
`/#/attention`, `/#/done`, `/#/feedback`, `/#/entries`, `/#/search`, `/#/new`, `/#/claims`, and
`/#/edit/P7@N`; `/#/` shows Overview. The Review tab is a static note: review packets stay a CLI
surface (`trufflepig board review P7@N`). A stored revision page `/#/P7@N` links the previous and
next revisions, the diff from its predecessor, the current plan, and History. The active navigation
item and tab carry `aria-current="page"`. Heading query state survives table-of-contents, same-page
Markdown, and skip-link navigation. Read selection and Project chips follow the
[Project contract](board-projects.md#browser-selector). Feedback uses 20-record FeedbackList pages
and batches at most four Entry Show reads to obtain each row's current permissions. Recent-call
metadata renders literally; typed outbox entries/events display `(spooled, unverified)`.

References link through `board_refs.js:boardRefRoute`, the one grammar for rendered links: `E485`
opens the entry; `P7`, `P7@N`, and `P7@A..B` open the plan, revision, or diff; `P7.3` opens
`/#/P7?tab=tasks&task=P7.3`; a worker address `P7.3/handle` adds `focus=worker-P7.3-handle`; a full
commit id opens its plan's Commits tab (`oid=`) when the plan is known and `/#/search?q=<oid>`
otherwise. Other text stays inert. A route's `focus=` names a `data-anchor` target (`entry-E485`,
`worker-P7.3-handle`, `commit-<oid>`), falling back to `heading`, then `oid`, then `task`. After a
navigation renders, the target's enclosing disclosures open, it scrolls into view, and it carries
`data-targeted`, an outline that fades (static under reduced motion). Live refreshes never repeat
this, and same-page focus links reveal without a reload.

Overview refreshes every 15 seconds: inbox renewal, claim expiry, and configuration changes can
occur without advancing the event sequence. Entries views
page newest-first with a composite before-cursor; each refresh re-reads the page with one entries
read, and there is no live tail or divider. The seen mark holds the highest delivered seq and
badges newer ticker rows; it lives at `localStorage["trufflepig-board-seen:<board-id>"]`.
Snapshot loads and delivered stream frames raise it monotonically — every write keeps the
stored maximum, so a lagging tab cannot lower it. A fresh baseline snapshot (first load, resync,
back/forward restore) may lower it to the fresh watermark, when the database was replaced
underneath the tab.
The ingest button holds the 202's ticket and completes from its matching `ingest` receipt. A
receipt arriving before the 202 reply is cached by ticket and consumed immediately on reply,
preserving its completion or failure notice. The button shows queued only while awaiting the
receipt; it reports unknown after 30 seconds without a matching receipt.

Board timestamps, including claim activity, revisions, commits, and freshness, display relative
time and tick locally each second without a read. Semantic `time` elements retain ISO dates;
hover titles show exact local dates, time through seconds, UTC offsets, and timezone names.
Future dates use `in 1m`; invalid or missing dates show `Unknown time`. Server timestamps use the
server clock offset; freshness uses the client clock. Shared timers stop on page hide and resume
once on page show; restored and foregrounded pages update their relative text immediately.

Form drafts, focus (by `data-focus-key`), scroll position, and native disclosures (by
`data-disclosure-key` and occurrence) survive live-region refreshes. Disclosure choices persist for
the tab's lifetime, including across navigation; a lazily filled disclosure renders its body when
restored open. Connection announcements fire only on outage, authorization expiry, and restore.
Editors retain the originally loaded base revision; stale edits preserve the user's draft and never
silently rebase or retry.
Proposal/acceptance authority and task transitions remain backend decisions.

## Active tasks and Done

Working now leads Overview with `Working now (N)`, an `all claims →` link, and one row per returned
current claim, stale claims first and then by latest activity. Each row is a native disclosure,
closed by default; its one-line summary (two lines on narrow screens) shows the task id and title
linking to the task, the ellipsized scope, the actor mark with model (and harness when the mark
does not name it), the step, open questions when nonzero, live workers, relative activity, and a
stale badge. The full scope and `user@host/harness/session` identity with model and effort stay in
hover titles and the mark's label. Expanded, a row lists each live worker (mark, `/handle` linking
to that worker, role, scope, activity), the ended-worker count, and `task details →`. Working now
never requires horizontal scrolling. Below it, Needs you precedes plan lanes on narrow screens.

Full claim cards — scope, attribution, signals, and the task agent tree — appear on Claims, a plan's
Tasks claims panel, Needs you stale claims (linking to their tasks without release controls), and
task detail. The task agent tree lists the lead claim and its workers with role, state, handle,
scope, files, last activity, review verdict, and progress. Its attribution adds the observed origin, peer IP, and
authority source recorded on the claim's entry. Workers ended more than 30 minutes ago collapse under
`Ended workers (N)`, and workers beyond the first 20 count as omitted.

A plan's Tasks tab shows each claim with the same attribution. A stale claim offers `Release stale
claim` (a `task_move` to `todo`) and omits the `todo` move control. A task reference such as
`/#/P7?tab=tasks&task=P7.1` opens that task's detail pinned above the task list: its agent tree,
claim history, task entries, and task-linked commits, with omitted counts. A Latest activity claim
event opens its task focused on the claim entry and still links the entry; worker events open the
worker. An entry page whose references name a task links `View in task`, focused on that entry.

Every plan page shows Plan grants below its tabs. Rows list roles, live, expired, or revoked status,
grantee, owner scope, source, grant time, and expiry. Where the server reports `can_manage_grants`,
managers see Revoke on unrevoked grants and a grant form for an exact session or harness-wide
recipient with expiry, note, and role checkboxes; other sessions see a read-only note. Role
checkboxes keep their checked state across live refreshes.

Needs you cards and Latest activity summaries preview at most 280 Unicode codepoints. Their entry
links retain access to the complete body, and entry pages display that body without truncation.
Latest activity displays newest events first, ordered by sequence for snapshots and live updates.
Display sorting preserves the source event order and pagination cursors.

Overview orders plans by their most recent recorded activity, newest first, across pages. The
Filters control includes a “Show plans without active tasks” checkbox, off by default. Plans with
only Done tasks or no tasks are hidden; filtering happens before pagination. The choice survives
refresh and navigation in the tab and is carried by `showInactive=0|1` in overview links. The
Filters disclosure retains its open state during refresh. The separate Done page includes
completed tasks regardless of this filter.

Overview prioritizes `todo`, `doing`, `review`, and `blocked` tasks in ordinal order within the
[collection bounds](board-cli-contract.md#collection-bounds). `tasks_omitted` counts additional
active tasks only; plan counts include those omitted tasks. The four active columns align to
their own content and reflow to two columns, then one, as the screen narrows. A plan without
active tasks displays a compact empty state instead of empty columns.

Done is a full-width footer below the active columns: a native `details` element with summary
`Done (<count>)`, closed by default, and up to five recent Done cards. Its link always opens the
plan's Done tab, displaying `N more →` when N (the count minus cards shown) is positive. The
disclosure's open or closed choice survives live refresh and navigation within the same tab;
reloading the page starts closed.

The plan Done tab is `/#/P7?tab=done`; `/#/done` reads Done tasks across the selected scope.
Completion fields and current-state paging follow the
[CLI contract](board-cli-contract.md#collection-bounds). Groups are Today, This week, and Earlier,
using browser-local calendar boundaries corrected by the server clock offset; the week starts
Monday. Plan pages obtain server time from Show; board pages use DoneTasks. Missing or nonfinite
`done_at` values belong to Earlier.

Load older follows the returned `next_before` cursor.

## Agent marks

Actor surfaces render inline 16px rounded SVG monograms for the `AgentVendor` values.
Module `marks/agent_marks.js` supplies these glyphs:

| Vendor | Glyph |
|---|---|
| `claude` | Cl |
| `codex` | Cx |
| `kimi` | Ki |
| `grok` | Gk |
| `gemini` | Ge |
| `qwen` | Qw |
| `muse` | Mu |
| `human` | Hu |
| `unknown` | ? |

Marks appear on claim badges, task holders, Working now, ticker rows, entries, task details, plan
claim markers, revision pages/history, and commit coauthors. Each has `role=img`, title, and
`aria-label`. Claim, entry, and event labels use `model · harness · user@host/session`, with
`unclaimed` for a missing model. Revision pages/history derive marks from the harness because they
store no model or vendor. Commit coauthor labels use `model · harness · email`, reflecting their
stored identity. Vendor attribution follows the [CLI rules](board-cli-contract.md#attribution).

Task cards prefer a current claim's mark. An assignee without a claim uses the harness from a full
`user@host/harness/session` identity, with an unclaimed model. An ambiguous bare recipient uses
`unknown` with `Assigned to <recipient>` as its label; an unassigned task has no assignee mark.

Light and dark themes supply `--mark-<vendor>` fills and `--mark-glyph`, maintaining at least
4.5:1 glyph contrast. `AGENT_MARKS` records contain `glyph`, `colorVar`, and `image: null`.
No vendor logo files are bundled. Asset routes follow the
[HTTP contract](board-web-contract.md#http-surface-and-typed-operations). Vendor logos require
Josh's brand approval for each vendor.

## Token bootstrap

The tab accepts a bootstrap token only from a non-route hash exactly `token=<64 lowercase hex>`,
removes it from the URL with `history.replaceState`, and retains it only in tab `sessionStorage`
and memory. A differing stored session is kept and flagged at load while the stray offer is held
in memory; the pending token is adopted without a second navigation when the stored session
expires. On later navigations, an expired or never-authorized tab adopts an offered `#token=`;
a live tab strips it at once without adopting it, flagging a differing offer and staying silent
when it matches the current token. A stream that cannot send for lack of a token stops without
reconnecting and reports authorization required.

## Stream leadership

One leader tab per origin and token generation (Web Locks `trufflepig-board-stream-<sha256-16>`)
holds the stream, heartbeats every 5 seconds, and relays every frame type over the matching
`BroadcastChannel`; followers show Live only on a heartbeat fresh within 10 seconds, steal the lock
after two missed beats, dedupe by `seq` on per-tab watermarks, and failover resumes from the new
leader's watermark — without Web Locks or BroadcastChannel each tab keeps its own stream, a bounded
incremental UTF-8/SSE parser with reconnect backoff, coalesced refreshes, and obsolete-route results
ignored. A hidden tab yields its stream and defers its election until shown, so a visible tab leads.
A 401/403 expires the tab: it releases the lock, drops its token, and announces
authorization-expired without retrying.

Tabs sharing one stream buffer relayed frames across snapshot reads: the channel stays open
while idle, frames with an `id` above the snapshot watermark apply in order once it lands, and
a buffer past 500 frames is dropped for a fresh resync — together closing the
snapshot/subscription race.
