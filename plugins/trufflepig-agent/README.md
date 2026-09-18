# Trufflepig agent integration

Shared search skill and audited CLI wrapper for Codex, Claude Code, Kimi Code, Muse Code, and omp.
The skill makes Trufflepig the default for project discovery, with targeted
fallbacks for unavailable or unsupported operations. It does not block shell
commands or require a Codex hook or MCP server.

## Install

Requires Python 3, `trufflepig` on `PATH`, and the wrapper destination on `PATH`.
CPU installation is `cargo install --path . --locked`; retain `--features
semantic-cuda` when updating an existing GPU installation.

```sh
plugins/trufflepig-agent/install.sh --codex --systemd --check "$PWD"
plugins/trufflepig-agent/install.sh --claude --check "$PWD"
plugins/trufflepig-agent/install.sh --project ~/Source/lunatic
plugins/trufflepig-agent/install.sh --omp --check "$PWD"
plugins/trufflepig-agent/install.sh --kimi --muse --kimi-hooks --systemd
```

`--claude` installs the personal skill, session hook, and narrow sandbox runtime
access while preserving existing settings. See [Claude integration](claude.md)
for discovery, session lifecycle, permissions, verification, and removal.

`--codex` symlinks the shared skill into `~/.agents/skills`, with automatic
selection enabled. `--project DIR` installs into `DIR/.agents/skills`; avoid
installing the same skill at both scopes for Codex. Existing links to this
checkout are reusable; conflicting unmanaged destinations fail without replacement.
`--bin DIR` changes the default `~/.local/bin` wrapper destination. With no
selectors, the installer attempts Kimi and Muse installation.

`--omp` installs the omp integration into omp's own config root, which is always
`~/.omp` (not governed by `XDG_CONFIG_HOME`): the shared skill under
`~/.omp/agent/skills` and a session-attribution extension under
`~/.omp/agent/extensions`, which omp auto-discovers. It shares the same wrapper
and skill as the other harnesses; running `--codex` and `--omp` together leaves
the skill at two paths, but omp deduplicates by skill name, so there is no
functional conflict. See [omp integration](omp.md) for discovery, session
lifecycle, verification, and removal.

Codex discovers skills by their description and also supports explicit
`$trufflepig-code-search` invocation. Start a fresh session if discovery does not
refresh. See [Codex skill discovery](https://learn.chatgpt.com/docs/build-skills).
Skill availability does not guarantee automatic selection on every prompt.

The Codex and Claude installers configure a data-only `agent-runtime.json` under
`$XDG_CONFIG_HOME/trufflepig` (default `~/.config/trufflepig`). Its `runtime_dir`
is `~/.cache/codex-tmp/trufflepig-agent` by default; `--runtime-dir DIR` selects
another disk-backed directory the sandbox permits writing. `spool_dir` is its
`spool` child. The wrapper supplies that spool to the CLI, and `--systemd` installs
it in the router's environment. Bare CLI callers can set `TRUFFLEPIG_SPOOL_DIR`
to the same path. Index databases retain their normal cache locations.

`--systemd` installs, enables, and restarts the user service. Stopping/restarting
it includes all service-owned child daemons, so use it at an upgrade checkpoint.
It does not change Codex sandbox permissions. `--check ROOT` searches ROOT as a singleton and reads
verified source through the installed wrapper; it fails on missing results or
runtime errors. Repeat the same smoke check inside Codex to verify sandbox access:

```sh
python3 plugins/trufflepig-agent/scripts/check_agent.py "$PWD"
trufflepig-audit --harness codex --since 1 --json
```

## Runtime recovery and upgrades

A daemon rejecting a flag such as `--format` while direct execution accepts it
indicates a client/daemon mismatch. Reinstall the binary and restart the router
**and its children**. The supplied unit uses `KillMode=control-group`; installer
reloads that policy before stopping the old service. Detached daemons started
outside the service require explicit shutdown in their owning root/workspace:

```sh
trufflepig --no-workspace --root /path/to/repository stop
trufflepig --workspace /path/to/workspace.toml stop
trufflepig semantic worker stop   # when upgrading the optional shared worker
plugins/trufflepig-agent/install.sh --codex --systemd --check "$PWD"
plugins/trufflepig-agent/install.sh --claude --check "$PWD"
```

Do not work around mismatches by stripping flags. The wrapper preserves CLI
errors rather than negotiating with old versions. Requests interrupted by an
upgrade may need retrying after the service is healthy.

If socket access is denied, the wrapper uses the configured router spool.
Check that the service has the same `TRUFFLEPIG_SPOOL_DIR`, that its `heartbeat`
is fresh, and that the harness can write that directory. A stale or absent heartbeat
means the router is unavailable. `--no-daemon` requires writable index/cache
access and is not the normal sandbox workaround. Do not place indexes, audit
fallbacks, or spool traffic on RAM-backed `/tmp`.

## Search and relationship limits

The [CLI contract](../../docs/cli.md) defines query syntax, budgets, coverage,
handles, and source verification. Plain search uses semantic/rerank lanes only
when requested or enabled in workspace settings. Empty results remain possible;
lexical-only search works without GPU support.

The skill uses `search → show` and adds `ctx`/`refs` when relationships help locate
the implementation. This reuses existing evidence; it does not automatically
attach a complete dependency neighborhood to each search result. The
[language contract](../../docs/language-contract.md) distinguishes resolved,
candidate, and unresolved relationships across Rust, TypeScript/JavaScript,
Luau, and DreamMaker. Docs/config remain text-searchable.

For example, DreamMaker `special_bucket` records explicit inheritance but its
context may have `target: null`. Read the declaration and search the named parent.
Context can also spend its budget on unresolved calls; prefer a narrower symbol
or explicit target search over repeatedly increasing the budget. Neither behavior
establishes compiler-equivalent binding resolution or automatic parent delivery.

## Attribution and audit

`trufflepig-agent` injects compact lines output, client, session, and detailed
diagnostics unless explicitly supplied. Codex markers identify the harness;
`CODEX_THREAD_ID` groups its calls across working directories, with
`CODEX_SESSION_ID` as a fallback. Explicit CLI `--client`/`--session` win over
`TRUFFLEPIG_AGENT_HARNESS`/`TRUFFLEPIG_SESSION`, which win over detection.
Claude attribution uses its session environment hook as described in
[Claude integration](claude.md). omp attribution identifies the harness from
`OMPCODE` or the parent process, then reads a per-cwd marker written by the omp
extension at session start, keyed by `sha256(cwd)` (Bun/Node expose no
`digest_size=6` blake2s, so omp uses sha256; other harnesses keep blake2s).
Kimi/Muse retain their own session variables and session-start markers; a marker
older than twelve hours is ignored. Without session information, attribution
falls back to a harness/directory/day identifier.

Each call logs arguments, exit code, latency, byte count, coverage, truncation,
and struggle signals under `$TRUFFLEPIG_AGENT_LOG_DIR`, otherwise
`$XDG_STATE_HOME/trufflepig/agent-audit` (default `~/.local/state/trufflepig`). If
unwritable, audit/recent-query state uses the configured runtime directory.
Without runtime configuration, fallback uses `$TMPDIR/trufflepig-<uid>` or
`~/.cache/codex-tmp/trufflepig-<uid>`; set `TMPDIR` to disk-backed storage.
`trufflepig-audit` reads primary and fallback logs without double counting a
shared directory. Logging does not change the CLI's stdout or exit code.

```sh
trufflepig-audit --harness codex --calls
trufflepig-audit --session SESSION --json
trufflepig audit SESSION
```

`TRUFFLEPIG_BINARY` selects the executable; `TRUFFLEPIG_AGENT_DIAGNOSTICS` selects
`detailed` (default), `metadata`, or `off`. Explicit `--json` returns fields omitted
by compact output. Wrapper audit still records query arguments independently of
the daemon diagnostics setting. Audit follow-through is a heuristic, not evidence
of task success or measured token savings.

## Other harness hooks

Kimi's optional `--kimi-hooks` and the Muse manifest use `hooks/session-start.sh`
for attribution and best-effort daemon startup. `hooks/steer-search.py` blocks
ordinary search until the first Trufflepig call only in registered workspaces
where that hook is installed; `TRUFFLEPIG_AGENT_STEER=nudge|off` changes this.
Codex and Claude installation do not install those steering hooks.

## Verification

```sh
python3 -m unittest discover -s plugins/trufflepig-agent/tests -p 'test_*.py'
python3 evaluation/navigation_replay.py --trufflepig target/debug/trufflepig
```

Use representative tasks with identical required source evidence to compare
ordinary tools with Trufflepig. Count captured output tokens with `o200k_base`
when available, calls, and completed evidence; do not substitute byte estimates
for missing tokenizer measurements or infer billed usage from replay results.
