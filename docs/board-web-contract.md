# Local board web contract

The board dashboard is a local human interface to the [durable board](board-contract.md).
`trufflepig board-serve --listen 127.0.0.1:7341` runs in the foreground; the listen
address defaults to that value and must be `127.0.0.1` or `::1`. `--no-daemon`
rejects serving verbs. The [agent installer](../plugins/trufflepig-agent/README.md)
provides opt-in `--board` user-service installation with `Restart=on-failure`.
The dashboard shares the router's absolute database and system runtime directory;
installation and standalone startup reject a conflicting known router database pin.

## Authority and bootstrap

Public HTML, JavaScript, and CSS contain no board records or authentication token.
Startup and the CLI print a bootstrap URL, `http://AUTHORITY/#token=TOKEN`; the
fragment stays outside the HTTP request. JavaScript immediately removes it with
`history.replaceState` and retains it only in tab `sessionStorage` and memory.
CLI bootstrap links use `/?ref=REF#token=TOKEN`, where REF is `P7`, `P7@N`, or
`E485`; bootstrap converts to a hash route and removes the reference query/token.
Every private JSON, render, ingest, and event-stream request supplies `X-Board-Token`.
Browser event streams use `fetch` streaming so they can supply that header.
The server atomically publishes API/address/database, without a token, in owned
regular 0600 `system::dir()/board-web.json`. `board web [P7|P7@N|E#]` validates
that descriptor, configured/known DB, and an authenticated Feed read (limit 1) before
printing the actual listener URL, including custom/ephemeral ports. Missing,
stale, or mismatched endpoints advise `board-serve`; it opens no browser and creates no token.

The stable token is 256 CSPRNG bits encoded as 64 hexadecimal characters, stored
at `system::dir()/board-web.token`. Runtime resolution is `TRUFFLEPIG_SYSTEM_DIR`,
then `$XDG_RUNTIME_DIR/trufflepig/system`, `$XDG_CACHE_HOME/trufflepig/system`, or
`$HOME/.cache/trufflepig/system`. A unique candidate uses
`create_new`/`O_NOFOLLOW`, mode 0600, write/fsync, and atomic no-replace hardlink
publication; the winner is validated and the parent directory synced. Candidate
filenames use an independent UUID rather than secret bytes. Existing tokens
must be a regular file owned by the uid with mode 0600. Unsafe or
malformed files fail closed; startup does not rotate a token automatically.
Authentication compares tokens in constant time.

`Host` accepts only `localhost`, `127.0.0.1`, or `[::1]` with the actual bound
port, including an ephemeral port; port 80 uses its canonical omitted form.
Every POST requires `Origin: http://HOST` exactly matching that validated request
Host; aliases cannot cross. A supplied GET Origin must also match. There is no CORS permission. Private replies use
`Cache-Control: no-store`. Request actor/model/effort/claims fields are rejected;
the server captures configured `user@host/human/web` identity with no agent claims.
The backend enforces owner-user plus human/steward authority and revision CAS.

## HTTP surface and typed operations

HTTP route version `v1` and typed `BOARD_API = 3` are independent contracts.
The public shell supplies the typed API value; every JSON mutation/read envelope
uses that value. Mismatches fail `board_api_mismatch` without negotiation.
Errors are `{ "error": { "code": CODE, "message": TEXT } }`.

| Method and route | Request or result |
|---|---|
| `GET /`, `/app.js`, `/app.css` | Public shell and root assets only |
| `GET /board_dom.js`, `/board_views.js`, `/board_details.js` | Public UI modules |
| `GET /board_feedback.js`, `/board_stream.js`, `/board_reader.js`, `/board_entries.js` | Public UI modules |
| `POST /api/v1/board` | `{ "api": 3, "op": BoardOp }`; typed `BoardReply` |
| `POST /api/v1/ingest` | `{ "api": 3 }`; authenticated router ingest relay |
| `GET /api/v1/render/plan/P7` or `P7@12` | `{api, revision, snapshot_seq, html, headings}` |
| `GET /api/v1/render/diff/P7@10..12` | `{api, before, after, hunks, snapshot_seq}` |
| `GET /api/v1/render/proposal/E80` | `{api, entry, before, hunks, snapshot_seq}` |
| `GET /api/v1/events?after=SEQ&plan=P7` | Authenticated SSE; plan filter optional |

The web allowlist exposes Overview, Show, Tasks, Claims, Feed, Attention, History, Entries,
Search, Review, and FeedbackList reads; New, Post, TaskCreate, TaskMove, Accept,
Reject, Edit, FeedbackClose, and FeedbackTriage writes. Browser Post kinds are
only `note`, `answer`, `decision`, and `question`. Other typed operations are
rejected. Ingest probes router API/database identity and relays without spawning
a router; an unavailable router is an error. Web Review labels stored evidence
without a new Git scan. Local reads remain available without a responsive router;
a conflicting known database pin rejects private reads, writes, and streams.

Read dispatch uses the shared `BoardOp::is_read_only` classifier, regardless of
HTTP method. Startup opens/migrates the writer before four query-only readers.
Each read captures owned records and `snapshot_seq` in one deferred transaction,
then releases its lease before rendering or network output. Unknown readers
create no actor/session records. Reader checkout, writer acquisition, and SQLite
busy waits use the remaining request deadline; poison recovery preserves permits.
Reloaded claim TTL applies to both writers and readers.

Collection bounds are part of the typed reply contract:

| Read | Bound and continuation |
|---|---|
| Feed | 500 events; ascending `seq`, scalar `next_after`, fixed `through` |
| History | 200 revision metadata records without bodies; same sequence cursor |
| Entries | 200 entries; ascending `(seq,id)`, composite `next_after: {seq,entry}` |
| Overview | 200 plans; `PlanId` cursor; 20 tasks and 20 active claims per plan, with omitted counts |
| Attention | 200 entries and 200 own stale claims; entry and claim continuation cursors, omitted counts |
| Tasks | 200 cards; `TaskId` cursor, captured `TaskCeiling`, and omitted count |
| Claims | 200 claims; composite `{entry,claim}` cursor and omitted count |
| Plan Show | 200 each tasks, active claims, entries, and commits, with omitted counts |
| Entry Show | 20 answer replies and 20 reverse references; cursors, `through`, and omitted counts |
| FeedbackList | 200 feedback records; composite `(seq,entry)` cursor, fixed `through`, omitted count |
| Search | 50 FTS hits, with truncation |

Sequence-based pages retain inclusive `through` as their source cutoff.
Mutable task, claim, and proposal states reflect each current transaction; every
reply's `snapshot_seq` is its actual transaction watermark. Entries filters by
plan, kind, harness, user, host, task, and referenced entry. Retain whole composite
cursors so entries sharing a sequence and claims sharing an entry are not skipped.
Attention returns `next_after` for entries and `claims_next_after` for own stale
claims; Tasks/Claims/Entries recover collections omitted from Plan Show.
Tasks starts with null `after`/`through`/`ceiling`, capturing `{plan,ordinal}`;
ordinal zero freezes an empty plan. Continuations require the whole ceiling plus
`after`/`through`: moved cards remain, newer ordinals stay out. PlanView and each
PlanOverview capture `task_ceiling` in the same transaction. Feedback preserves `open_only`.
Supplying Tasks `after` or `through` without a captured ceiling is invalid.
Overview/Attention/Plan expose `server_now` and `claim_ttl_secs`; Plan exposes
`can_edit`. Attention uses the actual request actor for recipients and own stale
claims, and `rebase_needed` names that actor's open proposals with stale bases.
Entry Show retains optional any-state proposal body/base/state and permissions;
closed proposals and superseded entries remain readable. Entry Show includes
feedback metadata and linked-commit detail. Bounded Entries with `references: E#`
recover reverse links; adding kind `answer` selects replies. Feedback browsing uses
20-record FeedbackList pages and batches of up to four Entry Show reads. Proposal diff
rendering returns complete shared-source hunks and the minimum of its immutable
entry/base read watermarks, preserving the snapshot/subscription race bound.

## Bounded HTTP transport

The server uses eight workers and a queue of 64 accepted connections. Acceptance
starts a whole-request five-second deadline including queue time; each read uses
only the remaining time. An overloaded accept loop does not block writing a busy
response. Request line plus headers fit 16 KiB, and the body fits 64 KiB with an
exact checked decimal `Content-Length`. POST requires that length and
`application/json` without parameters, matched case-insensitively.

The parser accepts one strict ASCII origin-form HTTP/1.1 GET or POST request with
CRLF lines. It rejects malformed header names, controls, obsolete folding,
whitespace before a colon, duplicate Host/Content-Length/Origin/X-Board-Token/
Content-Type, all Transfer-Encoding, Expect, Upgrade, GET bodies, premature EOF,
lone LF, and buffered/pipelined extra bytes. Each connection closes after its
response. Writes have a whole-response five-second deadline; SSE uses a separate
five-second deadline per frame.

Host mismatch returns 421; foreign/missing required Origin returns 403; missing or invalid
private token returns 403; unsupported/missing POST content type returns 403. Headers
include `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`.
CSP permits same-origin resources and sets `frame-ancestors 'none'`,
`base-uri 'none'`, `object-src 'none'`, and `form-action 'none'`.

## Snapshot, replay, and stream lifetime

Subscribe from the `snapshot_seq` returned by the initial read. Events replay
in database order with `seq > cursor` before waiting for new writes.
Pages composed from several reads require every root `snapshot_seq` and use
their minimum as the initial/resync stream cursor; `through` is only membership.
`Last-Event-ID` takes precedence over the query cursor; malformed cursors fail.
Each `board` event has `id: SEQ` and a plain `EventRecord` JSON payload.

A cursor ahead of the database, or a backlog above 500 events, sends a `resync`
event without an id and closes the stream. Reasons are `cursor_ahead` or
`replay_gap`, with the current `latest` watermark. Clients reload their snapshot
and reconnect from the new snapshot's watermark; they never advance to `latest`
without fetching the unseen state. This closes the snapshot/subscription race.

At most 32 subscribers hold RAII permits; handoff immediately releases the HTTP
worker. No reader, transaction, or writer lock survives a network send or wait.
A 250 ms poller observes cross-process sequence changes and coalesces generation
wakeups. Sequence-reader errors or panics mark the stream service unavailable,
close active streams, and reject new subscriptions with `board_unavailable`/503;
successful polling restores availability even if the sequence has not changed.
SSE is close-delimited with `Connection: close` and no Content-Length; keepalives
arrive every 15 seconds. Disconnect, failed spawn, panic, and slow writes release
permits. The browser runs one stream with a bounded incremental UTF-8/SSE parser
and reconnect backoff, coalesces refreshes, and ignores obsolete-route results.

## Browser state and safe rendering

Primary routes are `/#/P7`, `/#/P7@N`, `/#/P7@A..B`, and `/#/E485`. Named routes
include `/#/attention`, `/#/feedback`, `/#/entries`, `/#/search`, `/#/new`,
`/#/claims`, and `/#/edit/P7@N`; `/#/` shows Overview. Heading query state preserves the
route through table-of-contents, relative Markdown, and skip-link navigation.
Browser Overview/Attention are global; Attention and own-stale Claims use `all=true`.
Feedback uses 20-record FeedbackList pages and batches at most four Entry Show
reads to obtain each row's current permissions. Recent-call metadata renders
literally; typed outbox entries/events display `(spooled, unverified)`.
Overview refreshes every
15 seconds and claim ages tick locally each second: inbox renewal, claim expiry,
and configuration changes can occur without advancing the event sequence.
Form drafts and focus survive live-region refreshes independently.
Editors retain the originally loaded base revision; stale edits preserve the
user's draft and never silently rebase or retry. Proposal/acceptance authority
and task transitions remain backend decisions.

The server escapes raw Markdown HTML and inline HTML, suppresses images, and
validates decoded link destinations. Only HTTP(S), mailto, and relative links
are allowed; controls, backslashes, and ambiguous/obfuscated schemes are denied.
Generated attributes are escaped. Only server-sanitized markup reaches
`innerHTML`; entries, references, snippets, and FTS text use `textContent`.
Heading anchors are deterministic, including duplicate/formatted headings.
`task.section` matches normalized visible heading titles exactly; one task covers
every repeated heading with that title. Anchors identify locations, and task
navigation selects the first matching anchor. Uncovered titles are deduplicated
in source order.
