# Board feedback and durable lifetime

Feedback reports and their durable delivery for the [plan board](board-contract.md). Command grammar
lives in the [CLI contract](board-cli-contract.md); entry caps, identity, and error prefixes live in
the board contract.

## Filing, states, and metadata

File feedback only when failed, confusing, wrong, or missing Trufflepig behavior forces a
workaround. Its body records commands tried, observed result, workaround, and what would help. A
report is an entry, with optional plan and states `open`, `triaged`, `fixed`, `wontfix`, or
`duplicate`; closing records an event for the reporting session. Triage moves open to triaged;
terminal states never reopen. Plan feedback decisions require owner-user human/steward authority;
global feedback requires reporter-user human. Stored metadata includes version/build ID,
actor/claims, repo key and relative cwd, steering mode, and the last five audited wrapper calls.
Recent calls carry abbreviated args, exit/error prefix, truncation/coverage, at most 2048 encoded
bytes, and no source response bodies. Summary and body together fit the 4096-byte entry cap. Audit
redaction omits board body, scope, section, model, and recent-call text. Absolute cwd is omitted
unless enriched relative to a verified Git root. Feedback dedupe ignores
actor/session/claims/cwd/import UUID and keys kind, summary, body, and plan for ten minutes across
callers.

## Outbox and spool

If router/DB delivery fails, the client atomically writes a 0600 `<uuid>.feedback` in the spool and
reports `queued: pending import`. Only `*.request` files are claimed as requests; startup/orphan
cleanup preserves feedback records. Idle import deduplicates UUIDs transactionally, removes files
only after commit, and quarantines permanent semantic errors. Constraint and type-mismatch
storage failures quarantine as invalid state, too-big as invalid body; corrupt
and not-a-database storage stays pending with retry backoff from two to 60 seconds, as do
unavailable/locked imports;
quarantine time resets on quarantine without following symlinks. Imported entries show
`via=outbox`, unverified spooled provenance; quarantines expire after 30 days, while pending
`.feedback` has no expiry. If the spool is unwritable, `board_unavailable` reports the failure.
