# Board web UI behavior

Browser companion to the [local board web contract](board-web-contract.md); server routes,
authentication, stream lifetime, and markup sanitizing live there. Token bootstrap and cross-tab
stream leadership are below.

Primary routes are `/#/P7`, `/#/P7@N`, `/#/P7@A..B`, and `/#/E485`. Named routes include
`/#/attention`, `/#/done`, `/#/feedback`, `/#/entries`, `/#/search`, `/#/new`, `/#/claims`, and
`/#/edit/P7@N`; `/#/` shows Overview. The Review tab is a static note: review packets stay a CLI
surface (`trufflepig board review P7@N`). Heading query state survives table-of-contents, same-page
Markdown, and skip-link navigation. Read selection and Project chips follow the
[Project contract](board-projects.md#browser-selector). Feedback uses 20-record FeedbackList pages
and batches at most four Entry Show reads to obtain each row's current permissions. Recent-call
metadata renders literally; typed outbox entries/events display `(spooled, unverified)`.

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

Form drafts and focus (by `data-focus-key`) survive live-region refreshes; connection announcements
fire only on outage, authorization expiry, and restore. Editors retain the originally loaded
base revision; stale edits preserve the user's draft and never silently rebase or retry.
Proposal/acceptance authority and task transitions remain backend decisions.

## Active tasks and Done

Overview prioritizes `todo`, `doing`, `review`, and `blocked` tasks in ordinal order within the
[collection bounds](board-cli-contract.md#collection-bounds). `tasks_omitted` counts additional
active tasks only. Its Done column is a collapsed native `details` element with summary
`Done (<count>)` and up to five recent Done cards. An `N more →` link opens the plan's Done tab,
where N is the count minus cards shown.

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
