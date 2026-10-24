# Trufflepig agent integration

Shared search and plan board skills with an audited CLI wrapper for Codex, Claude Code,
Grok Build, Gemini CLI, Kimi Code, Muse Code, omp, and OpenCode. The [plan board
skill](skills/trufflepig-plan-board/SKILL.md) coordinates claims, revisions, commits, and
feedback; the [web contract](../../docs/board-web-contract.md) defines the dashboard. The
search skill makes Trufflepig the default for project discovery, with targeted fallbacks for
unavailable or unsupported operations. Optional hooks steer shell searches toward it (see
[Other harness hooks](#other-harness-hooks)); no MCP server is required.

## Install

Needs Python 3, `trufflepig` and wrappers on `PATH`. Use `cargo install --path . --locked`;
keep `--features semantic-cuda` for GPU updates. Before writes, `--board-api-version` must match
[API](../../docs/board-contract.md#backend-transport-and-deadlines) in 5 s. Existing configured DBs
or `--board` need [current schema](../../docs/board-contract.md#storage-configuration-and-search).
Otherwise any `board_error` warns. Managed restarts need that API in one 10 s connect/read deadline.
Router absence or activation failure rolls back; only unmanaged preflight permits absence.

```sh
plugins/trufflepig-agent/install.sh --codex --systemd --check "$PWD"
plugins/trufflepig-agent/install.sh --claude --check "$PWD"
plugins/trufflepig-agent/install.sh --grok --gemini --opencode --muse --check "$PWD"
plugins/trufflepig-agent/install.sh --project ~/Source/lunatic
plugins/trufflepig-agent/install.sh --omp --check "$PWD"
plugins/trufflepig-agent/install.sh --kimi --muse --kimi-hooks --systemd
```

`--claude` installs the personal skills, session hook, Bash search-steering hooks,
a `Bash(trufflepig-agent *)` permission, and narrow sandbox runtime access while
preserving existing settings. See [Claude integration](claude-code.md)
for discovery, session lifecycle, permissions, verification, and removal.

`--grok`, `--gemini`, `--opencode` link both skills: [Grok](grok.md), [Gemini](gemini.md), [OpenCode](opencode.md).
`--muse` refreshes managed copies through `muse skills install`; rerun after source updates.
`--kimi` links both under `$KIMI_CODE_HOME/skills` (default `~/.kimi-code/skills`).
See [Kimi K3 Board setup](skills/trufflepig-plan-board/references/kimi.md).

`--codex` symlinks both skills into `~/.agents/skills`, with automatic selection enabled.
`--project DIR` installs into `DIR/.agents/skills`; avoid installing the same skill at both
scopes for Codex. Existing links to this checkout are reusable; conflicting unmanaged
destinations fail without replacement. `--bin DIR` changes the default `~/.local/bin` wrapper
destination. With no selectors, the installer attempts Kimi and Muse installation.

`--omp` links both skills and a session-attribution extension under omp's active agent directory
(default `~/.omp/agent`), honoring its environment overrides and profiles; `--omp-agent-dir DIR`
selects an explicit destination. See [omp integration](omp.md) for its lifecycle and removal.

Codex discovers skills by their description and also supports explicit `$trufflepig-code-search`
invocation; start a fresh session if discovery does not refresh. See [Codex skill
discovery](https://learn.chatgpt.com/docs/build-skills). Availability does not guarantee selection.

The skill's optional [rustdoc helper](skills/trufflepig-code-search/references/rustdoc.md) adds
Rust API documentation and trait navigation in Codex: it builds one Cargo target with an installed
nightly and returns bounded JSON with provenance and source spans, with no extra setup.

Codex, Claude, Grok, Gemini, and OpenCode configure a data-only `agent-runtime.json` under
`$XDG_CONFIG_HOME/trufflepig` (default `~/.config/trufflepig`). `runtime_dir` defaults to
`~/.cache/codex-tmp/trufflepig-agent`; `--runtime-dir DIR` selects another disk-backed
directory the sandbox permits writing. `spool_dir` is its `spool` child. The wrapper supplies
that spool to the CLI, and `--systemd` installs it in the router's environment. Bare CLI
callers set `TRUFFLEPIG_SPOOL_DIR` to the same path. Index databases keep normal cache locations.

`--systemd` installs the router; opt-in `--board` installs the foreground dashboard
on a stable loopback port; optional LAN access follows the [web security contract](../../docs/board-web-security.md). Both pin
the same absolute board database and system runtime/token directory. Reinstalling the router
restarts it and starts the board only if it was running; `trufflepig board web` prints the
current bootstrap URL. Router restarts include all service-owned children. `--check ROOT`
searches ROOT as a singleton and reads verified source; repeat inside Codex to verify sandbox
access. Missing results or runtime errors fail:

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

If socket access is denied, the wrapper uses the configured router spool. The
service and harness must share `TRUFFLEPIG_SPOOL_DIR`; the harness needs write
access and a fresh `heartbeat`. `--no-daemon` skips the router. Index-backed
searches need a usable index and selected result cache; `show path:FILE[:START-END]`
and opaque `read:path:` continuations read current bytes without either. Keep
indexes, audit fallbacks, and spools off RAM-backed `/tmp`.

## Search and relationship limits

The shared [search skill](skills/trufflepig-code-search/SKILL.md) explains filter use,
pagination, and linked-worktree fallback; the [CLI contract](../../docs/cli.md)
defines query syntax, budgets, coverage, handles, and source verification. Plain
search uses semantic/rerank lanes only when requested or enabled in workspace
settings. Empty results remain possible; lexical-only search works without GPU support.

The skill uses `search → show` and adds `ctx`/`refs` when relationships help locate
the implementation. This reuses existing evidence; it does not automatically
attach a complete dependency neighborhood to each search result. The
[language contract](../../docs/language-contract.md) distinguishes resolved,
candidate, and unresolved relationships across Rust, TypeScript/JavaScript, C#,
PHP, Luau, and DreamMaker. Docs/config remain text-searchable.

For DreamMaker inheritance with `target: null`, read the declaration and search
the named parent. When unresolved calls exhaust context, narrow the symbol or
search an explicit target; this does not establish compiler-equivalent binding.

## Attribution and audit

`trufflepig-agent` defaults line output, detailed diagnostics, and a client label. Native markers
and process ancestry select the harness before the fallback hint. When several native markers are
present, as in a nested harness that inherits its parent's markers, the nearest ancestor naming a
marked harness wins. Kimi and Muse markers are their session variables, `KIMI_SESSION_ID` and
`MUSE_SESSION_ID`, not any variable with that prefix. Detected harnesses cannot claim
`human` or `cli`. Board actor session precedence is CLI `--session`, `TRUFFLEPIG_SESSION`, native
provider ID, then `AGENT_SESSION_ID`; absent all, the actor session is unattributed. Origin preserves
the observed harness/session despite a CLI session claim. Diagnostic baselines come only from
explicit `--session` or `TRUFFLEPIG_SESSION`. Codex prefers `CODEX_THREAD_ID` over
`CODEX_SESSION_ID`; Claude/omp/OpenCode integrations supply provider IDs per call. No session ID is
synthesized; `trufflepig-audit` groups calls without one as `unattributed`. See
[Claude integration](claude-code.md) and [omp integration](omp.md).

Each call logs arguments, exit code, latency, byte count, coverage, truncation, and struggle
signals. Board/feedback text is redacted. Logs use `$TRUFFLEPIG_AGENT_LOG_DIR`, otherwise
`$XDG_STATE_HOME/trufflepig/agent-audit` (default `~/.local/state/trufflepig`). If unwritable,
audit/recent-query state uses the configured runtime directory. Without runtime configuration,
fallback uses `$TMPDIR/trufflepig-<uid>` or `~/.cache/codex-tmp/trufflepig-<uid>`; set `TMPDIR`
to disk-backed storage. `trufflepig-audit` reads primary and fallback logs without double
counting a shared directory. Logging does not change the CLI's stdout or exit code.

```sh
trufflepig-audit --harness codex --calls
trufflepig-audit --session SESSION --json
trufflepig audit SESSION
```

`TRUFFLEPIG_AGENT_MODEL`/`TRUFFLEPIG_AGENT_EFFORT` supply claims only to board/feedback.
`TRUFFLEPIG_BINARY` selects the executable; `TRUFFLEPIG_AGENT_DIAGNOSTICS` selects `detailed`
(default), `metadata`, or `off`. Explicit `--json` returns fields omitted by compact output.
Wrapper audit still records query arguments independently of the daemon diagnostics setting.
Audit follow-through is a heuristic, not evidence of task success or measured token savings.

## Other harness hooks

Kimi's optional `--kimi-hooks` and the Muse manifest use `hooks/session-start.sh`
for best-effort daemon startup. The hook writes no session state.

`hooks/steer-search.py` steers ordinary search in checkouts of registered workspace members and
Git roots whose default cache holds an index, including linked worktrees. It classifies each shell
search (`definition`, `body`, `outline`, `references`, `regex`, `concept`, `files`) and source read
(`read`: `sed -n A,Bp F` → `show path:F:A-B`, `cat F` → `map F`), resolving paths against
`cd DIR`/`git -C DIR`, and names the equivalent command (prefixed with `cd CHECKOUT &&` when that
is another checkout). Glob tool calls are file searches. Pipe filters, copies (`cat F > out`), logs
and command output, other revisions, filesystem `find` actions, shell variables, and paths outside
the checkout are never steered. Nudges are full for an agent's first search of a class, one line
after; piped or chained `trufflepig-agent` calls get a tip that the footer was lost. Modes: `off`
does nothing; `nudge` allows and adds the equivalent command as context after the search (Claude
default); `block` denies until one Trufflepig call, with or without a session, from this directory
in 45 minutes (Kimi/Muse default); `strict` denies `definition`, `body`, `outline`, and
`references` with the equivalent command unless a Trufflepig search, `refs`, `map`, or `show` from
this checkout returned no hits or failed in the last 10 minutes, or the command ends with
`# tp-fallback: reason`; it nudges the rest, including `read`.

`TRUFFLEPIG_AGENT_STEER` overrides `steer.<harness>` in `agent-runtime.json`, which
`install.sh --steer MODE` records for the selected Claude, Kimi, or Muse installation. Symbol,
body, outline, and `refs` commands are suggested only for Rust, TypeScript, JavaScript, C#, PHP,
Luau, and DreamMaker; docs paths and other languages get `re:` equivalents, and bare all-caps
markers such as `TODO` are not steered. Codex, Grok, Gemini, omp, and OpenCode do not install
steering hooks and report steering as `off`.

`trufflepig-audit --adoption` compares Trufflepig navigation calls with the ordinary searches the
hook observed, split into main agent and subagents, with the classes that escaped. Blocked
searches and `read`s are reported separately.

## Verification

```sh
python3 -m unittest discover -s plugins/trufflepig-agent/tests -p 'test_*.py'
python3 evaluation/navigation_replay.py --trufflepig target/debug/trufflepig
```

Use tasks with identical required source evidence to compare ordinary tools with Trufflepig.
Count output tokens with `o200k_base` when available, calls, and completed evidence; do not
substitute byte estimates for missing tokenizer measurements or infer billed usage from replay results.
