# Board web UI behavior

Browser companion to the [local board web contract](board-web-contract.md); server routes,
authentication, stream lifetime, and markup sanitizing live there. Token bootstrap follows that
contract's authority section; cross-tab stream leadership is below.

Primary routes are `/#/P7`, `/#/P7@N`, `/#/P7@A..B`, and `/#/E485`. Named routes include
`/#/attention`, `/#/feedback`, `/#/entries`, `/#/search`, `/#/new`, `/#/claims`, and `/#/edit/P7@N`;
`/#/` shows Overview. The Review tab is a static note: review packets stay a CLI surface
(`trufflepig board review P7@N`). Heading query state survives table-of-contents, same-page
Markdown, and skip-link navigation. Browser Overview/Attention are global; Attention and own-stale
Claims use `all=true`. Feedback uses 20-record FeedbackList pages and batches at most four Entry
Show reads to obtain each row's current permissions. Recent-call metadata renders literally; typed
outbox entries/events display `(spooled, unverified)`.

Overview refreshes every 15 seconds and claim ages tick locally each second: inbox renewal, claim
expiry, and configuration changes can occur without advancing the event sequence. Entries views
page newest-first with a composite before-cursor; each refresh re-reads the page with one entries
read, and there is no live tail or divider. The seen mark holds the highest delivered seq and
badges newer ticker rows; it lives at `localStorage["trufflepig-board-seen:<board-id>"]`.
Snapshot loads and delivered stream frames raise it monotonically — every write keeps the
stored maximum, so a lagging tab cannot lower it. Only a resync lowers it, to the fresh
snapshot watermark, when the database was replaced underneath the tab.
The ingest button holds the 202's ticket, shows queued, then completes only on the `ingest` frame
with that ticket or reports unknown at 30s.

Form drafts and focus (by `data-focus-key`) survive live-region refreshes; connection announcements
fire only on outage, authorization expiry, and restore. Editors retain the originally loaded
base revision; stale edits preserve the user's draft and never silently rebase or retry.
Proposal/acceptance authority and task transitions remain backend decisions.

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
