# Cache and daemon

The cache defaults to `$XDG_CACHE_HOME/trufflepig/<root-hash>`, or
`$HOME/.cache/trufflepig/<root-hash>`. `--cache DIRECTORY` overrides it. Each cache
belongs to one canonical root; use disk-backed storage for the index and staging
database and avoid RAM-backed `/tmp`. A linked Git worktree owns its own cache,
seeded from its main checkout's default cache when that cache holds an index
(`src/store/seed.rs`). The router evicts a default-base cache whose recorded
root no longer exists, on startup and every ten minutes, after a one-minute grace
period and only when the root's parent directory still exists (`src/system/sweep.rs`);
`system prune` runs that sweep now. Explicit `--cache` bases are never swept.
History defaults to the normal cache base
keyed by canonical Git common directory, so linked worktrees share immutable Git
facts. An explicit `--cache` isolates history; `--history-cache DIRECTORY` selects
a shared history-cache base. In workspace mode, `--cache` supplies an isolated
base with coordinator state and separate member caches.
A per-user system daemon gives agent harnesses one endpoint to allowlist. Its
socket is `daemon.sock` in `$TRUFFLEPIG_SYSTEM_DIR` verbatim, else
`$XDG_RUNTIME_DIR/trufflepig/system` (the allowlist target), else
`$XDG_CACHE_HOME/trufflepig/system`, else `$HOME/.cache/trufflepig/system`.
The router forwards each request to the owning workspace coordinator or per-root
daemon, starting a missing target and proxying the reply within 28 s
(`src/daemon.rs:PROXY_REPLY_WAIT`). A router reply, including an error, is the
answer; only an unreachable router falls back to the per-root/coordinator path,
then local dispatch. Daemons issue their own sub-requests (workspace owner verbs,
member `semantic` commands) directly (`src/cli.rs:run_direct`), never through the
router that may still be proxying the request. `stop`,
`index`, `init`, `ws`, `semantic status`, `semantic-check`, the `*-serve` verbs, and
`--no-daemon` requests never touch it. `system ensure` starts the router, `system
stop` shuts it down, `system status` reports if it runs, `system prune` evicts
caches for vanished roots immediately, and `system-serve` serves it in the
foreground. The agent plugin ships a systemd user unit (`install.sh
--systemd`) that keeps the router running and restarts it on failure. Service stop/restart
includes its child daemons so binary upgrades do not retain old processes; its
session-start hook starts that service, or straps a detached router when the unit is
absent. A sandbox whose seccomp filter denies unix-socket connects (Muse's does) cannot
reach any socket, and its read-only cache also blocks `--no-daemon`; such clients reach
the router through its file spool instead. The router claims each `<request>.request`
file in `$TRUFFLEPIG_SPOOL_DIR`, else `/tmp/trufflepig-<uid>/spool`, by renaming it
to `<request>.claimed`, answers it on a worker with `<request>.reply`, and refreshes a `heartbeat`
file every second; a client whose socket connect fails spools its request only while
that heartbeat is under five seconds old, and otherwise reports `workspace_unavailable`.
The [Codex integration](../plugins/trufflepig-agent/README.md) configures a shared,
disk-backed spool for its wrapper and router; do not use the legacy `/tmp` default
on machines with RAM-backed temporary storage.
Singleton commands start a per-root index daemon automatically. Workspace queries
start a coordinator unless `--no-daemon` is set; `ws show` and `ws status` inspect
locally. The coordinator starts member daemons when needed and reads their published
indexes. Semantic requests may start the shared per-user inference worker when enabled.
`--no-daemon` performs local indexing and launches or contacts no background process,
including the inference worker. Search then uses published indexes and cached
semantic vectors only. The explicit foreground path `semantic prepare --no-daemon`
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
Every daemon answers requests concurrently: its accept thread hands each
connection to a bounded worker pool (`src/daemon/pool.rs`: router 16 workers,
coordinator 8, root 4, each queueing 64), and a full queue answers
`daemon_busy: retry`. One maintenance thread (`src/daemon/reconciler.rs`) watches,
reconciles on watch events and periodically, drains the spool, and runs idle
probes. A root daemon binds and serves at once, before any database work; its
initial reconcile creates the schema, and until the first publication reads
answer `index_warming` (explicit path reads still serve the current file). The maintenance thread lowers itself to nice 10 and idle I/O
after that initial reconcile; request workers keep normal priority. Each request's
20 s query deadline starts when it is accepted. Clients wait at most 30 s for any
reply (`src/daemon.rs:CLIENT_REPLY_WAIT`, socket and spool alike) and retry a read
verb once, with a fresh request id, on a timed-out read, `daemon_busy`,
`database is locked`, or, after 2 s, `index_warming` (`src/cli/retry.rs`).
Workspace `index` and `init` run in the client, never on a coordinator worker.
Daemons have no build-version negotiation. Stop them before changing binaries. Very long cache
paths can exceed Unix socket limits; use a shorter `--cache` path. Watcher fallback
and resource limits are documented in the [index contract](index-contract.md).
