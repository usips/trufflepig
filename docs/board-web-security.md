# Board web access and provenance

The [web service](board-web-contract.md) accepts connections only from loopback or IPv4
`192.168.0.0/16`. IPv4-mapped IPv6 peers are normalized to IPv4 before admission and recording.
Other private networks, public addresses, and non-loopback IPv6 peers are rejected before
HTTP parsing or worker admission. A rejected peer receives 403 and the connection closes.

The installed service binds `127.0.0.1:0` by default. To enable LAN access, run `board-serve
--listen 0.0.0.0:0`, or bind an explicit `192.168` address. For a user service, override its
`ExecStart` with the same installed binary and that listen argument, reload systemd, and restart
`trufflepig-board.service`. A service drop-in survives installer refreshes. The stable-port
file and descriptor follow the [web service contract](board-web-contract.md).

LAN mode serves plain HTTP, so the board token (owner-level web access) and every board reply cross
the network unencrypted. Use a non-loopback listener only on trusted networks. Restarting
`board-serve` rotates the token and invalidates links from earlier starts.

## Authority and bootstrap

Public HTML, JavaScript, and CSS contain no board records or token. `board-serve` prints the full
bootstrap URL `http://AUTHORITY/#token=TOKEN` only when stdout is a terminal, and otherwise the
origin plus advice to run `trufflepig board web`, which prints the fresh URL. Wildcard listeners
publish their actual bind address but use `127.0.0.1` for local links and proof connections.
For LAN access, replace the loopback host in a fresh bootstrap link with the machine's
`192.168` address, retaining the port and fragment. The fragment stays outside HTTP requests;
browser adoption follows the [UI contract](board-web-ui.md#token-bootstrap).

CLI bootstrap links `/?ref=REF#token=TOKEN` (REF `P7`, `P7@N`, or `E485`) convert to a hash route
and drop the reference query/token. Every private JSON, render, ingest, and event-stream request
supplies `X-Board-Token`; browser streams use `fetch` streaming to send it. The server atomically
publishes API/address/database, without a token, in owned regular 0600
`system::dir()/board-web.json`, removed on shutdown.

`board web [P7|P7@N|E#]` validates the descriptor and configured database, verifies the listener's
owner through `/proc/net/tcp{,6}`, and proves token possession before printing a link. The proof
never sends the token: the client POSTs a random nonce to `POST /api/v1/challenge`, which answers
hexadecimal HMAC-SHA256(key = token, `trufflepig-board-listener-proof-v1` NUL ‖ nonce ‖ actual
listener address). Wildcard proof connections use loopback while the proof remains bound to the
published wildcard address. The challenge route keeps Host, Origin, and JSON body checks.
Missing, stale, or mismatched endpoints advise `board-serve`; `board web` creates no token.

The token is 256 CSPRNG bits stored as 64 hexadecimal characters at
`system::dir()/board-web.token`. Every start rotates it using `O_NOFOLLOW`, 0600 permissions, and
atomic rename, so restart invalidates old links. Unsafe or malformed files are replaced without
writing through them; candidate filenames use an independent UUID, and the parent directory is
synced. Runtime resolution follows the [runtime contract](runtime-contract.md). Authentication
compares tokens in constant time.

`Host` accepts `localhost`, `127.0.0.1`, or `[::1]` with the actual bound port. A wildcard
listener also accepts canonical IPv4 literals in `192.168.0.0/16`; a specific LAN listener accepts
its bound LAN address. These authorities require the actual port. Port 80 uses its
canonical omitted form. Every POST requires `Origin: http://HOST` matching the validated Host;
a supplied GET Origin must also match. Comparisons are case-insensitive; aliases cannot cross.
There is no CORS permission. Private replies, the shell, and assets use `Cache-Control: no-store`.

Request actor/model/effort/claims/origin fields are rejected. The server uses configured
`user@host/human/web` identity with no agent claims and applies the
[board authority rules](board-contract.md#identity-references-and-authority).

## Observed IP and request audit

Every web Board action receives server-created origin metadata with the direct socket peer IP.
Persisted entries and events retain that IP, and attribution displays it alongside the observed
harness and session. Historical records without IP remain unknown. `Forwarded` and
`X-Forwarded-For` headers never determine admission or provenance.

Each HTTP interaction writes a bounded structured record to stderr, captured by the service
journal: peer IP, method, route category, and response status. Records cover admission refusals,
malformed or unauthorized requests, queue pressure, reads, writes, and stream admission. They
exclude request bodies, arbitrary paths, query strings, fragments, tokens, and headers.
Use `journalctl --user -u trufflepig-board.service` to inspect access records.
