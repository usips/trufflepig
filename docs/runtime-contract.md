# Cache and daemon

The cache defaults to `$XDG_CACHE_HOME/trufflepig/<root-hash>`, or
`$HOME/.cache/trufflepig/<root-hash>`. `--cache DIRECTORY` overrides it. Each cache
belongs to one canonical root; use disk-backed storage for the index and staging
database and avoid RAM-backed `/tmp`. A standalone linked Git worktree owns a
separate cache that can seed from the main checkout's default cache when it holds
an index (`src/store/seed.rs`). Workspace cache selection follows the
[workspace contract](workspace-contract.md#retrieval-and-output). A worktree cache
records its root in a `cache-root` marker until it holds an index; `ws status`
reports a workspace worktree's seed outcome. The router evicts a
default-base cache whose recorded root no longer exists, on startup and every ten minutes,
after a one-minute grace period and only when the root's parent directory still exists
(`src/system/sweep.rs`);
`system prune` runs that sweep now. Explicit `--cache` bases are never swept.
History defaults to the normal cache base
keyed by canonical Git common directory, so linked worktrees share immutable Git
facts. An explicit `--cache` isolates history; `--history-cache DIRECTORY` selects
a shared history-cache base. In workspace mode, `--cache` supplies an isolated
base with coordinator state and separate member caches.
A per-user system daemon gives agent harnesses one endpoint to allowlist. Its
socket is `daemon.sock` in the system directory (`src/system.rs:dir_from`):
`$TRUFFLEPIG_SYSTEM_DIR` when absolute, else
`$XDG_RUNTIME_DIR/trufflepig/system` (the allowlist target) when absolute, else
`/run/user/<uid>/trufflepig/system` when that login-session directory exists, is
owned by the caller, and is mode 0700, else `$XDG_CACHE_HOME/trufflepig/system`,
else `$HOME/.cache/trufflepig/system`. Empty and relative values fall through at
every step, and with no absolute candidate there is no directory
(`system_unavailable`). `trufflepig system dir` prints the resolved directory.
For source operations, the router forwards requests to the owning workspace coordinator or per-root
daemon, starting a missing target and proxying the reply within 28 s
(`src/daemon.rs:PROXY_REPLY_WAIT`), spawn wait included. Router replies, including
`timed_out` errors, remain answers. An unreachable router, a `daemon_unavailable`
reply (the owner could not be reached or started), or a locally expired
file-spool deadline falls back to the per-root/coordinator path, then local
dispatch (`src/cli.rs:run_with_context`). Daemons issue their
own sub-requests (workspace owner verbs, member `semantic` commands) directly
(`src/cli.rs:run_direct`), never through the router that may still be proxying
the request, and wait for them only within the request's query deadline. `stop`,
`index`, `init`, `ws`, `semantic status`, `semantic-check`, the `*-serve` verbs, and
`--no-daemon` requests never touch it. `system ensure` starts the router, `system
stop` shuts it down, `system status` reports if it runs, `system prune` evicts
caches for vanished roots immediately, and `system-serve` serves it in the
foreground. The agent plugin ships a systemd user unit (`install.sh
--systemd`) that keeps the router running and restarts it on failure. Service stop/restart
includes its child daemons so binary upgrades do not retain old processes; its
session-start hook starts that service, or straps a detached router when the unit is
absent. Installation treats a refused Unix-socket connection as an absent unmanaged
router; managed service readiness still requires a live compatible reply
(`plugins/trufflepig-agent/scripts/install_agent.py:router_endpoint_present`).
A sandbox whose seccomp filter denies unix-socket connects (Muse's does) cannot
reach any socket. Its read-only cache can block indexed `--no-daemon` operations;
explicit current-path reads bypass the index and cache. Other routed requests
reach the router through its file spool. The router claims each `<request>.request`
file in `$TRUFFLEPIG_SPOOL_DIR`, else `/tmp/trufflepig-<uid>/spool`, by renaming it
to `<request>.claimed`, answers it on a worker with `<request>.reply`, and refreshes a `heartbeat`
file every second; a client whose socket connect fails spools its request only while
that heartbeat is under five seconds old, and otherwise leaves the request unspooled.
The [Codex integration](../plugins/trufflepig-agent/README.md) configures a shared,
disk-backed spool for its wrapper and router; do not use the legacy `/tmp` default
on machines with RAM-backed temporary storage.
Index-backed singleton commands start a per-root index daemon automatically.
Index-backed workspace queries start a coordinator unless `--no-daemon` is set; `ws show` and `ws status` inspect
locally. The coordinator starts member daemons when needed and reads their published
indexes. Semantic requests may start the shared per-user inference worker when enabled.
For source operations, `--no-daemon` launches or contacts no background process,
including the inference worker. Current-path `show` reads and `read:path:`
continuations read contained bytes without indexing or saving a result set;
other indexed reads may use bounded inline maintenance. Search then uses published
indexes and cached semantic vectors only. The explicit foreground path
`semantic prepare --no-daemon`
holds the root preparation lease while it runs. Explicit `index` reconciles locally;
`status` reports existing indexed coverage. `serve` runs the singleton index daemon
in the foreground and `stop` requests shutdown. In workspace mode, `stop` stops only
the coordinator; stop a member daemon with `trufflepig --no-workspace --root ROOT
stop`. Match `--root` and `--cache` to the daemon being controlled. Daemon startup
also launches a leased history worker without awaiting indexing. `hist-index --wait`
continues history batches until completion or explicit failure. `doctor` runs bounded
integrity/provenance probes without starting inference. The index daemon attempts
probes while idle. `semantic status` reports root preparation state; `semantic worker
status` reports shared-worker residency.
Every daemon assembles frames incrementally without blocking its accept loop,
with pending capacity 128 at the router and 64 at a root daemon or coordinator.
An incomplete frame expires after 5 s with `timed_out: phase=reading_frame`;
two ingress workers decode and classify complete frames, and classification may
perform I/O (`src/daemon/server.rs`, `src/daemon/server/ingress_frames.rs`).
Request work then enters bounded lanes (`src/daemon/pool.rs`,
`src/daemon/server/request_admission.rs`): query
lanes have 16 router, 8 coordinator, and 4 root workers, each with a 64-request
queue. Maintenance, regex, and semantic lanes have router widths/queues 16/32,
16/32, and 8/16; coordinator 4/8, 4/8, and 2/8; and root 1/8, 2/8, and 1/8.
Admission refusal returns `daemon_busy`. A panicking request
answers `internal_error: MESSAGE` and its worker keeps serving. Router and
coordinator cap queued-plus-active maintenance, regex, and semantic commands per
canonical root at 2, 3, and 2. Overlapping workspace scopes share that root key;
the permits cover scheduling, forwarding, and reply wait, not the lifetime of
background preparation (`src/daemon/server/root_work_limits.rs`). Root execution
widths for those lanes are 1, 2, and 1.
One maintenance thread (`src/daemon/reconciler.rs`) watches, reconciles on watch
events and periodically, drains the spool, and runs idle probes; if it panics,
the daemon exits so the next client starts a fresh one. A root daemon binds and
serves before database work; until its initial reconcile publishes, reads answer
`index_warming` (`more`, `ctx`, handle, and path reads still work). The maintenance
thread lowers itself to nice 10 and idle I/O after that reconcile; request workers
keep normal priority.

Forwarding has accepted per-hop budgets of 28 s at the router, 24 s at a
coordinator, and 20 s at a root daemon. The wire carries no absolute deadline,
so these budgets do not form one cross-process deadline. Live search, navigation,
and source operations cap local execution at 20 s and honor the incoming expiry;
read-only Board operations use the same local cap after parsing, while writes and
`ingest` keep the incoming deadline
(`src/board/board_transport/board_backend/board_dispatch/host_run.rs:local_read_deadline`). Query-only
index reads cap lock waits and interrupt long SQL statements at expiry. Deadline
checks are cooperative; a blocking system call can outlive the budget
(`src/daemon/deadline.rs`).
Inline index maintenance uses at most 2 s per root/member and reserves 250 ms from
the original read budget; workspace member slices share the original 20 s deadline
([index contract](index-contract.md#coherent-publication)).
The accepted budgets and pending-frame limits are set in `src/daemon/server.rs`.
This does not make every interactive read verb deadline-preemptible: the current
history dispatch calls `History::open`/`index` without `QueryDeadline`. Explicit
`index` and `init` maintenance runs uncapped once started. A spooled request's
accepted-tier budget is reduced by its publication age; if it has expired, the
router reports `timed_out` before classification.
`stop` releases the socket, drains accepted work for up to
28 s, then rejects queued work and returns so the daemon process exits; requests
still running at that point may lose their reply. Liveness checks connect to the
socket (`src/daemon.rs:running`) and never touch the startup lock. `system
ensure` uses one absolute 120 s startup deadline for status probes, spawn, and
socket/spool polling. Its initial status probe waits at most 250 ms; a bound
router then gets the remaining startup budget without a second spawn. Polling
uses the spool even when no socket runtime directory resolves. It starts no
probe or process after expiry and reports
`system_unavailable: router still starting after 120 s`. Clients allow at most
30 s for the read/retry operation, including startup, transport, backoff, and
fallback, and at most five read attempts with fresh request IDs
(`src/daemon.rs:CLIENT_REPLY_WAIT`, `src/cli/retry.rs`). A spooled
request whose claiming router stops beating fails at once with
`daemon_unavailable`. Lane queue refusals include pool, queue depth/capacity,
cause, bounded `retry_after_ms`, and a retry hint. Read retries
cover explicit `daemon_busy`, database contention, dropped connections,
`index_warming`, raw `WouldBlock`/`TimedOut` only when an attempt took under 5 s,
and typed reply timeouts. A `daemon_reply_timeout: phase=awaiting_reply` can be
retried when the shared budget leaves 1 s for a useful attempt after backoff; only an explicit
busy read also preserves a 250 ms local-fallback reserve. A service `timed_out`
answer, including frame timeout, is final. Current-path `show` and
`read:path:` continuation can fall back locally when cache opening fails before
dispatch with `index_cache_unavailable`, or after retry exhaustion returns an
explicit `daemon_busy`, within the read's deadline. Timeout, database-lock, and
ambiguous transport errors never trigger that fallback. Board and feedback
contention-only writes allow at most two attempts after explicit pre-dispatch
busy or database contention; other writes have no retry guarantee
(`src/cli/retry.rs`).
Workspace `index` and `init` run in the client, never on a coordinator worker.
Daemons have no build-version negotiation. Stop them before replacing binaries.
On Linux, `router_binary_replaced` identifies the owning process, executable, and
cache (`src/background_process.rs`). Repair only that owner. Rerun a replaced
foreground command with the current binary. Restart the router's owning unit with
`systemctl --user restart trufflepig-system.service`, or for a detached router run
`system stop` then `system ensure`. For a root or coordinator owner, restart its
unit or stop its detached daemon with that root and cache before retrying. Very long cache paths can exceed Unix socket limits; use a shorter
`--cache` path. Watcher fallback and resource limits are documented in the
[index contract](index-contract.md).

## Durable board

The router answers `board` and `feedback` in-process before workspace resolution,
using its lazy board host. It starts no source-index daemon for these commands.
Router idle schedules Git ingestion separately from the maintenance loop and
imports durable feedback outbox records. Board data lives in the data directory,
outside cache sweeps and `forget-logs`. Storage, transport fallback, waiter limits,
retry safety, and protocol errors follow the [board contract](board-contract.md).
