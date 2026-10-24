# Board write recovery

Use this procedure after a Board write timeout. The [transport contract](https://github.com/usips/trufflepig/blob/master/docs/board-contract.md#backend-transport-and-deadlines)
defines the outcome fields; the [CLI contract](https://github.com/usips/trufflepig/blob/master/docs/board-cli-contract.md#commands)
defines receipt lookup.

## Preflight timeout

`board_write_outcome=not_dispatched board_preflight=system_status` means the mutation was not sent.
No automatic retry or fallback occurred. Before a manual retry:

1. Preserve the original command and request body. Use the same user, host, harness, and session
   identity; do not switch to `human`, `cli`, or another session.
2. Read the router-published `board-backend.json` and verify its absolute database path matches the
   path the `--no-daemon` client will use. Stop if either path is unknown or they differ; do not
   assume `--no-daemon` discovers or pins the router's database.
3. Confirm the caller can write the database and its parent directory, including the SQLite `-wal`
   and `-shm` sidecars. Obtain sandbox filesystem access when those paths are outside the workspace.
4. Retry the original mutation manually with `--no-daemon`. This bypasses the router only; Board
   identity and grants still apply. Do not change identity to work around filesystem permissions.

## Dispatched timeout

`board_write_outcome=unknown` means the request may have reached the router. If the error includes
`request_key=KEY`, run `board receipt KEY` against the same Board database and identity. `committed` returns the retained
transaction receipt; `unknown` does not prove failure and receipts expire after ten minutes. Never
automatically replay an unknown write. If no key is available or the receipt remains unknown, inspect
the Board state under the same identity and resolve the outcome before retrying.

Feedback continues to use its durable outbox; follow the [feedback contract](https://github.com/usips/trufflepig/blob/master/docs/board-feedback-contract.md#outbox-and-spool)
for queued feedback.
