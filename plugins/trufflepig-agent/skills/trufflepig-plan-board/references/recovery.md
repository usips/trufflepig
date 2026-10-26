# Board write recovery

Use this procedure after a Board write timeout or a typed admission refusal. The [transport contract](https://github.com/usips/trufflepig/blob/master/docs/board-contract.md#backend-transport-and-deadlines)
defines outcome fields; the [CLI contract](https://github.com/usips/trufflepig/blob/master/docs/board-cli-contract.md#commands)
defines receipt lookup.

## Preflight timeout

`board_write_outcome=not_dispatched board_preflight=system_status` means the mutation was not sent.
No automatic retry or fallback occurred. First resend the same command after the reported
`retry_after_ms`; it is safe because nothing was written. If the router stays unresponsive, use the
[manual direct retry](#manual-direct-retry) requirements before adding `--no-daemon`.

## Router Board queue refusal

The client marks a write `board_write_outcome=not_dispatched` for `daemon_busy` only when the typed
error carries the complete router Board admission detail defined by the
[transport contract](https://github.com/usips/trufflepig/blob/master/docs/board-contract.md#backend-transport-and-deadlines).
A bare `daemon_busy`, another pool or cause, incomplete or duplicate required fields, or prose that
merely resembles the detail does not prove that a mutation was refused before dispatch.

The CLI keeps its bounded backoff for this typed refusal. It does not switch the mutation to direct
local execution. If bounded retry still receives the refusal, use the [manual direct retry](#manual-direct-retry)
requirements before adding `--no-daemon`.

## Manual direct retry

1. Preserve the exact command and request body. Keep the same user, host, harness, and session; do not
   switch to `human`, `cli`, or another session.
2. Read the router-published `board-backend.json` and verify its absolute database path matches the
   database selected by the `--no-daemon` client. Stop if either path is unknown or they differ; do
   not assume `--no-daemon` discovers or pins the router's database.
3. Confirm write access to the database and its parent directory, including the SQLite `-wal` and
   `-shm` sidecars. Obtain sandbox filesystem access when those paths are outside the workspace.
4. Manually run the original mutation with `--no-daemon`. This bypasses the router only; Board
   identity and grants still apply. The client never performs this direct write automatically.

## Dispatched timeout

`board_write_outcome=unknown` means the request may have reached the router. If the error includes
`request_key=KEY`, run `board receipt KEY` against the same Board database and identity. `committed`
returns the retained transaction receipt; `unknown` does not prove failure and receipts expire after
ten minutes. Never automatically replay an unknown write. If no key is available or the receipt
remains unknown, inspect the Board state under the same identity and resolve the outcome before
retrying.

Feedback continues to use its durable outbox; follow the [feedback contract](https://github.com/usips/trufflepig/blob/master/docs/board-feedback-contract.md#outbox-and-spool)
for queued feedback.
