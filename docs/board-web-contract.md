# Local board web contract

The board dashboard is a local human interface to the [durable board](board-contract.md).
`trufflepig board-serve` runs in the foreground; `--listen` selects a `127.0.0.1` or `::1` address.
The installed unit passes `--listen 127.0.0.1:0`: a port-0 bind retakes the port recorded in
`system::dir()/board-web.port` (plain decimal, 0600, re-recorded at every bind and never removed
on exit), falling back to an ephemeral port after any recorded-port bind error. It reports
`board-serve: recorded port N unusable (<error>); using M`. Clients discover the actual bound
port through `board-web.json`.
`--no-daemon` rejects serving verbs. The [agent
installer](../plugins/trufflepig-agent/README.md) provides opt-in `--board` user-service
installation with `Restart=on-failure`. The dashboard shares the router's absolute database and
system runtime directory; installation and standalone startup reject a conflicting router pin.

## Authority and bootstrap

Public HTML, JavaScript, and CSS contain no board records or token. `board-serve` prints the full
bootstrap URL `http://AUTHORITY/#token=TOKEN` only when stdout is a terminal, and otherwise the
origin plus advice to run `trufflepig board web`, which prints the fresh URL. The fragment stays
outside the HTTP request; the browser's token bootstrap and adoption rules live in the [board web
UI contract](board-web-ui.md#token-bootstrap). CLI
bootstrap links `/?ref=REF#token=TOKEN` (REF `P7`, `P7@N`, or `E485`) convert to a hash route and
drop the reference query/token. Every private JSON, render, ingest, and event-stream request
supplies `X-Board-Token` (browser streams use `fetch` streaming to send it). The server atomically
publishes API/address/database, without a token, in owned regular 0600
`system::dir()/board-web.json`, removed on shutdown (drop guard plus SIGTERM/SIGINT/SIGHUP/SIGQUIT).

`board web [P7|P7@N|E#]` validates that descriptor and the configured/known DB, verifies the
listener's owner through `/proc/net/tcp{,6}`, and proves token possession before printing the
listener URL. The proof never sends the token: the client POSTs a random nonce to the
unauthenticated `POST /api/v1/challenge` route, which answers hexadecimal HMAC-SHA256(key =
token, `trufflepig-board-listener-proof-v1` NUL ‖ nonce ‖ listener address); the client checks
that proof against the token it reads locally. The challenge route keeps the Host, Origin, and
JSON body checks. Missing, stale, or mismatched endpoints advise `board-serve`; `board web` opens
no browser and creates no token.

The token is 256 CSPRNG bits (64 hexadecimal characters) stored at `system::dir()/board-web.token`.
`board-serve` rotates it on every start — 32 fresh bytes written with `O_NOFOLLOW`/0600 discipline
and atomically renamed over any previous or planted entry — so a leaked token dies with the service
that published it. Unsafe or malformed files are replaced, never written through; candidate
filenames use an independent UUID, never secret bytes, and the parent directory is synced. Runtime
resolution follows the [runtime contract](runtime-contract.md) system directory; authentication
compares tokens in constant time.

`Host` accepts only `localhost`, `127.0.0.1`, or `[::1]` with the actual bound port, including an
ephemeral port; port 80 uses its canonical omitted form. Every POST requires `Origin: http://HOST`
matching that validated request Host; a supplied GET Origin must also match. Host and Origin↔Host
comparisons are case-insensitive; aliases cannot cross. There is no CORS permission; private
replies, the shell, and static assets send `Cache-Control: no-store`. Request
actor/model/effort/claims fields are rejected — the server captures configured `user@host/human/web`
identity with no agent claims; the backend enforces owner-user plus human/steward authority and
revision CAS.

## HTTP surface and typed operations

HTTP route version `v1` and the typed
[`BOARD_API`](board-contract.md#backend-transport-and-deadlines) value are independent contracts.
The public shell supplies the typed API value; every JSON mutation/read envelope uses that value.
Mismatches fail `board_api_mismatch` without negotiation. Errors are
`{"error":{"code":CODE,"message":TEXT}}`.

| Method and route | Request or result |
|---|---|
| `GET /`, `/board_web_main.js`, `/board_web.css` | Public shell and root assets only |
| `GET /board_dom.js`, `/board_views.js`, `/board_cards.js` | Public UI modules |
| `GET /board_routing.js`, `/board_render_loop.js` | Public UI modules |
| `GET /pages/board_pages.js`, `/pages/plan_page.js`, `/pages/proposal_page.js` | Page modules |
| `GET /pages/done_page.js` | Completed-task page module |
| `GET /stream/board_stream.js`, `/stream/stream_parse.js` | Stream modules |
| `GET /stream/stream_election.js` | Stream modules |
| `GET /feedback_triage.js`, `/board_reader.js`, `/board_entries.js` | UI modules |
| `GET /state/board_lru.js`, `/state/board_seen.js` | State modules |
| `GET /state/board_web_token.js` | Token state module |
| `POST /api/v1/challenge` | Unauthenticated ownership proof; Host/Origin/JSON checks apply |
| `POST /api/v1/board` | `{api: BOARD_API, op: BoardOp, project?: string}`; `BoardReply` |
| `POST /api/v1/ingest` | `{ "api": BOARD_API }`; single-flight router ingest relay |
| `GET /api/v1/render/plan/P7` or `P7@12` | `{api, revision, snapshot_seq, html, headings}` |
| `GET /api/v1/render/diff/P7@10..12` | `{api, before, after, hunks, snapshot_seq}` |
| `GET /api/v1/render/proposal/E80` | `{api, entry, before, hunks, snapshot_seq}` |
| `GET /api/v1/events?after=SEQ&plan=P7` | Authenticated SSE; plan filter optional |

Render route targets decode only `%40` (to `@`); any other percent escape fails `invalid_options`.
The optional top-level `project` selects a host Project for scoped reads; see the
[read scope contract](board-projects.md#read-scopes) for resolution and conflicting-scope errors.

The web allowlist exposes Overview, Repositories, Projects, Show, Tasks, DoneTasks, Claims, Feed,
Attention, History, Entries, Search, and FeedbackList reads; New, Post, TaskCreate, TaskMove,
Accept, Reject, Edit, FeedbackClose, and FeedbackTriage writes. Browser Post kinds are `note`,
`answer`, `decision`, and `question`. Other operations fail
`invalid_options: op not available over web`. Local reads remain available without a responsive
router; a conflicting known database pin rejects private reads, writes, and streams.

POST ingest probes router API/database identity and relays without spawning a router; an
unavailable router is an error. Single-flight, it answers 202 with `ingest:"queued"` plus a
flight `ticket` at once; a POST landing mid-scan takes the next ticket and queues one
rerun under it, and every scan restarts the router reply deadline. Completion reaches
subscribers as `event: ingest` frames (receipt or error envelope, ticket attached, no
`id:` line); the last eight receipts replay to every (re)subscribing stream.

Startup never migrates an existing database. It opens the writer only when a same-API router's
`system status` confirms `schema_file` equals `schema_supported` at the schema this binary
supports, when the file already holds that schema, or when no database exists yet; a stale file
waits up to 30 s, polling every 500 ms, for a concurrently-starting router to migrate it, then
refuses. An answering router gets `router is migrating`, a silent one gets `system ensure` advice;
a newer schema is unsupported — a reachable router disagreeing on API or database identity is
fatal. Read dispatch uses the shared `BoardOp::is_read_only` classifier, regardless of HTTP method.
Startup opens the writer before four query-only readers; a dedicated stream-feed reader
outside the pool serves the poller and ring fills, so N streams cost one read per active
poll tick, zero when idle, and never check out the pool. Each read captures owned records
and `snapshot_seq` in one deferred transaction, then releases its lease before rendering
or network output. Unknown readers create no actor/session records; reader checkout, writer
acquisition, and SQLite busy waits use the remaining request deadline; poison recovery
preserves permits; reloaded claim TTL reaches writers and readers.

Collection bounds are part of the [typed reply contract](board-cli-contract.md#collection-bounds).

Sequence pages retain inclusive `through` as their source cutoff; mutable task, claim, and proposal
states reflect the current transaction, whose watermark is every reply's `snapshot_seq`. Entries
filters by plan, kind, harness, user, host, task, and referenced entry, newest-first: `before`
continues older, `after` pages ascending (legacy), never both. Retain whole composite cursors so
same-sequence entries and same-entry claims are not skipped. Attention returns `next_after` for
entries and `claims_next_after` for own stale claims; Tasks/Claims/Entries recover collections
Plan Show omits. Ordinal Tasks start with null `after`/`through`/`ceiling`, capturing
`{plan,ordinal}` in the same transaction as PlanView and each PlanOverview's `task_ceiling`;
ordinal zero freezes an empty plan. Continuations require the whole ceiling plus `after`/`through`:
moved cards remain, newer ordinals stay out. Feedback preserves `open_only`; ordinal Tasks
`after`/`through` without a captured ceiling is invalid. Recent-first Tasks and DoneTasks follow
the linked collection bounds. Overview/Attention/Plan expose
`server_now` and `claim_ttl_secs`; Plan exposes `can_edit`. Attention uses the actual request actor
for recipients and own stale claims; `rebase_needed` names that actor's stale-base open proposals.
Entry Show retains optional any-state proposal body/base/state and permissions, feedback metadata,
and linked-commit detail; closed proposals and superseded entries remain readable. Bounded Entries
with `references: E#` recover reverse links; adding kind `answer` selects replies. Proposal diff
rendering returns complete shared-source hunks and the minimum of its immutable entry/base read
watermarks, preserving the snapshot/subscription race bound.

## Bounded HTTP transport

The server uses eight workers and a queue of 64 accepted connections. Transient accept failures
(aborted handshakes, resource pressure) skip to the next connection. Acceptance starts a
whole-request five-second deadline including queue time; a two-second first-byte timeout governs
only the initial read; an overloaded accept loop does not block writing a busy response. Request
line plus headers fit 16 KiB, and the body fits 160 KiB with an exact checked decimal
`Content-Length`. POST requires that length and `application/json` without parameters, matched
case-insensitively.

The parser accepts one strict ASCII origin-form HTTP/1.1 GET or POST request with CRLF lines. It
rejects malformed header names, controls, obsolete folding, whitespace before a colon, duplicate
Host/Content-Length/Origin/X-Board-Token/Content-Type, all Transfer-Encoding, Expect, Upgrade, GET
bodies, premature EOF, lone LF, and buffered/pipelined extra bytes. Each connection closes after its
response; after an early 400/413/431 the server sends the reply, shuts down its write side, and
drains briefly so the reply arrives first. Writes have a whole-response five-second deadline; SSE
frames carry their own five-second deadline.

Host mismatch returns 421; a foreign/missing required Origin, missing/invalid private token, or
unsupported/missing POST content type returns 403; an unsupported method returns 405 with `Allow`
naming the permitted methods. Replies carry `X-Content-Type-Options: nosniff` and
`Referrer-Policy: no-referrer`. CSP permits same-origin resources and sets `frame-ancestors 'none'`,
`base-uri 'none'`, `object-src 'none'`, and `form-action 'none'`.

## Snapshot, replay, and stream lifetime

Subscribe from the `snapshot_seq` returned by the initial read. Events replay from a shared
500-entry ring of pre-framed frames (plan filters replay its annotated relevance) in database order
with `seq > cursor` before waiting for new writes. Pages composed from several reads require every
root `snapshot_seq` and use their minimum as the initial/resync stream cursor; `through` is only
membership. `Last-Event-ID` takes precedence over the query cursor; malformed cursors fail. An
unknown `plan=` filter is rejected with 4xx before any 200. Each `board` event has `id: SEQ` and
a plain `EventRecord` JSON payload. Stream responses carry `Referrer-Policy: no-referrer` and
the shared CSP (`default-src 'self'`; `frame-ancestors`, `base-uri`, `object-src`, `form-action`
`'none'`). A backlog older than the ring's oldest frame, or a cursor above the observed watermark
after a ~600 ms grace, sends a `resync` event without an id and closes the stream; reasons are
`replay_gap` or `cursor_ahead`, with the current `latest` watermark. Clients reload their snapshot
and reconnect from the new snapshot's watermark; they never advance to `latest` without fetching
the unseen state (follower relay buffering: [stream leadership](board-web-ui.md#stream-leadership)).

At most 32 subscribers hold RAII permits; handoff immediately releases the HTTP worker, and
disconnect, failed spawn, panic, or slow writes release permits. No reader, transaction, or writer
lock survives a network send or wait. A 250 ms poller observes cross-process sequence changes and
coalesces generation wakeups; poller errors or panics mark the service unavailable, close active
streams, and reject new subscriptions with `board_unavailable`/503 until polling recovers. SSE is
close-delimited with `Connection: close`, no Content-Length, and keepalives every 15 seconds.
Client tab leadership — elections, heartbeats, expiry — follows [stream
leadership](board-web-ui.md#stream-leadership).

## Browser state and safe rendering

Browser behavior — routes, paging, the seen mark, ingest state, focus, and announcements — follows
the [board web UI contract](board-web-ui.md). The server escapes raw Markdown HTML and
inline HTML, suppresses images, and validates decoded link destinations. Only HTTP(S), mailto, and
same-page `#…` anchors are allowed — other relative links render inert; controls, backslashes, and
ambiguous/obfuscated schemes are denied. Generated attributes are escaped. Only server-sanitized
markup reaches `innerHTML`; entries, references, snippets, and FTS text use `textContent`. Heading
anchors are deterministic, including duplicate/formatted headings. `task.section` matches normalized
visible heading titles exactly; one task covers every repeated heading with that title. Anchors
identify locations, and task navigation selects the first matching anchor. Uncovered titles are
deduplicated in source order.
