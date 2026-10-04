# Board web UI behavior

Browser companion to the [local board web contract](board-web-contract.md); server routes,
authentication, stream lifetime, and markup sanitizing live there. Token bootstrap and the
cross-tab stream relay follow that contract's authority and stream sections.

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
page newest-first with a composite before-cursor, and live rows sit above a divider for entries new
since the page loaded. The seen mark `localStorage["trufflepig-board-seen"]` holds the highest
delivered seq and badges newer ticker rows. The ingest button shows queued on 202, then the
terminal receipt or error from the `ingest` stream frame.

Form drafts and focus (by `data-focus-key`) survive live-region refreshes; connection announcements
fire only on outage and restore. Editors retain the originally loaded base revision; stale edits
preserve the user's draft and never silently rebase or retry. Proposal/acceptance authority and
task transitions remain backend decisions.
