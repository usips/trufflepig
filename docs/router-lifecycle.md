# Router lifecycle

The per-user router ([runtime contract](runtime-contract.md)) runs either under
the plugin's systemd user unit or detached from `system ensure`. This contract
covers how each kind replaces its binary, what clients do while an owner runs a
replaced binary, and where the router places the daemons it spawns
(`src/system/router_lifecycle.rs`, `src/system/daemon_scope.rs`).

## Supervision

A router is supervised when its environment has
`TRUFFLEPIG_ROUTER_SUPERVISOR=systemd`, which the unit sets, and a non-empty
`INVOCATION_ID`, which systemd sets for every service invocation. Neither alone
counts: a shell inside some other unit inherits `INVOCATION_ID`, and the marker
without an invocation is not a systemd start. The router removes the marker from
every daemon it spawns. Any other router is detached. `system status` reports
`lifecycle.supervisor` as `systemd` or `detached`.

## Binary replacement

On Linux a background process cannot spawn children from a replaced executable
inode. Spawns then fail with `router_binary_replaced`, naming the owner,
executable, and cache (`crates/tp-core/src/background_process.rs`).

A supervised router samples its startup executable path once a second from its
maintenance thread. The path is replaced when its device and inode differ from
the running image (`/proc/self/exe`). A replacement settles once one sample,
identical in identity, size, and modification time, has held for 30 s and names
a regular, executable ELF file. A missing path, a script, or a file still being
written never settles. A settled replacement makes the router:

1. record the restart in `router-restarts` in its runtime directory;
2. stop accepting connections and release its socket, so new clients fail fast;
3. finish accepted frames and drain accepted requests for up to 28 s, the
   ordinary `stop` drain;
4. ask each root daemon and coordinator it spawned to stop, waiting up to 5 s
   for their sockets to close;
5. log `router_restart:` and exit with status 75 (`EX_TEMPFAIL`).

The unit has `Restart=on-failure`, `RestartSec=2`, and
`RestartForceExitStatus=75`, so systemd starts the new binary two seconds later.
`KillMode=control-group` and the daemon scopes' `BindsTo=` (below) end any old
daemon that outlived step 4. Clients that connect during the drain or restart
follow the normal unreachable-router path: one `system ensure`, then per-root
or local execution.

### Loop guard

At most three automatic restarts happen in any 600 s window. `router-restarts`
holds their Unix times and survives router generations, not reboots. A settled
replacement that would exceed the limit, or whose restart cannot be recorded,
keeps today's behavior: spawns refuse with `router_binary_replaced`, the journal
gets one `automatic restart ... suppressed` line naming the count and the wait,
and `system status` reports `lifecycle.binary_restart: suppressed` with
`recent_restarts`. The router keeps checking and restarts once the oldest
recorded restart leaves the window. A new binary that fails at startup is
bounded by systemd's own start limit instead.

### Detached routers and manual recovery

A detached router never exits for a replacement. Repair only the owner the
error names. Rerun a replaced foreground command with the current binary. For a
detached router run `trufflepig system stop`, then `trufflepig system ensure`,
which starts the installed unit when one owns the runtime directory. A detached
router holding the daemon lock while the unit is installed blocks the unit: stop
it with `trufflepig system stop` (or kill its PID), then run
`systemctl --user restart trufflepig-system.service`. For a root or coordinator
owner, restart its unit or stop its detached daemon with that root and cache
before retrying. Stop detached daemons before replacing their binary.

## Client read fallback

`router_binary_replaced` is refused before dispatch and has no side effects.
When a routed read — `search`, `refs`, `map`, `show`, `more`, `ctx`, or
`status` — gets it from the router, named in any layer or in relayed text
(`tp_core::coded_error::named_codes`), the client reruns the request in the
foreground with `--no-daemon` (`src/cli/client_command.rs`). Every other
command, including `board`, `feedback`, `hist-index`, and `semantic prepare`,
returns the refusal with its recovery hint.

## Process placement

Indexing never runs in the router process. Root daemons (`serve`) reconcile on
their maintenance thread and prepare semantic vectors on a preparation thread;
history workers and the shared inference worker are their child processes;
coordinators start member daemons. All are separate processes the router or its
children spawn, so process-level cgroup placement applies without threaded
cgroups.

A supervised router whose unit sets `TRUFFLEPIG_DAEMON_SLICE` passes that slice
and its own cgroup to each daemon it spawns. Before binding, such a daemon,
while still in the router's cgroup, calls the user manager's
`StartTransientUnit` through `busctl --user` to move itself into
`trufflepig-daemon-PID.scope` in that slice, bound with `BindsTo=` to the router
unit. It waits up to 500 ms for the move so its own children inherit the scope.
Grandchildren already in a scope never move. Without `busctl`, a user bus, or
the variables, the daemon stays in the router's cgroup.

The installed `app-trufflepig.slice` sets `CPUWeight=10` and `IOWeight=10`; the
router unit sets neither, so the router and board host keep the default weight.
Because the slice is a sibling of desktop applications under `app.slice`,
background daemons as a group stay below one desktop application however many
there are, as the whole unit did before. `IOWeight` takes effect only where the
user manager delegates the `io` controller.

Inside a daemon, request workers keep the process priority while the
maintenance thread (after its initial reconcile), the semantic preparation
thread, and the history worker lower themselves to nice 10 and idle I/O
(`crates/tp-core/src/background_priority.rs`), so searches served by a root
daemon run ahead of its indexing.

### Mechanism choice

- **Delegation** (`Delegate=yes` with sub-cgroups of the router unit) nests the
  daemons inside the router's own weight. A nested group never drops below the
  unit's siblings, so indexing would rise to a full application's share while
  the router idles, and moving processes across units violates systemd's
  delegation rules.
- **`systemd-run --scope` wrappers** around each spawn put the daemon in its
  scope before exec, but a missing bus or `systemd-run` would fail every spawn.
  Self-placement keeps spawning independent of systemd.
- **Per-thread priority alone** keeps the whole unit at one weight against the
  desktop, either starving the router (low) or lifting indexing (normal).
  It remains inside each daemon, where it orders requests ahead of indexing.

Transient scopes need no unit files per daemon. The slice and `BindsTo=` keep
the existing contract that stopping or restarting the router unit ends every
daemon it spawned. `MemoryHigh=` or `MemorySwapMax=` on the slice could bound
indexer memory and swap; they are not set because the limits depend on the
machine and a hard swap limit trades swapping for reclaim stalls or OOM kills.

## Installation

`install.sh --systemd` writes `app-trufflepig.slice` beside the router unit.
With `--systemd` or `--board` it removes `10-nice.conf` drop-ins of the router
and board units that only set scheduling or weight keys (`Nice=`, `CPUWeight=`,
`IOWeight=`, and similar); it keeps any other file and warns when a
`10-nice.conf` sets more. After `daemon-reload` it resets runtime CPU weights
left by `systemctl set-property --runtime` with `CPUWeight=`. A failed install
restores the drop-ins, the slice, and the runtime weights
(`plugins/trufflepig-agent/scripts/systemd_priority.py`).
