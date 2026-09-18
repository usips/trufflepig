# omp integration

## Installation and discovery

```sh
plugins/trufflepig-agent/install.sh --omp --check "$PWD"
```

`--omp` installs into omp's own config root, which is always `~/.omp` (not
governed by `XDG_CONFIG_HOME`):

- the shared skill at `~/.omp/agent/skills/trufflepig-code-search`, and
- a session-attribution extension at
  `~/.omp/agent/extensions/trufflepig-session.ts`.

omp scans both directories on startup and loads top-level `.ts`/`.js`
extension files, including symlinks, without a manifest. Both links point into
this checkout, so keep the checkout available. Start a new omp session after
installation so the skill is discovered and the extension loads.

omp selects the skill from its description; availability does not guarantee
selection on every prompt. omp also scans `~/.agents/skills` and
`~/.claude/skills`, so a Codex or Claude install already exposes the skill to
omp; `--omp` alongside them leaves the skill at two paths, and omp keeps one
skill per name.

## Session attribution

omp's shell tool exports `OMPCODE=1` to child processes but no session
identifier, so the marker file is the identity channel, the same pattern as
Kimi and Muse. The extension writes the session id to a per-cwd marker:

```
$XDG_STATE_HOME/trufflepig/agent-sessions/omp/<key>
```

`XDG_STATE_HOME` defaults to `~/.local/state`. The key is the shared
`cwd_key()` contract in `bin/trufflepig-agent`: SHA-256 of the working
directory, first twelve hex digits, computed identically by the Python
wrapper, the Kimi/Muse shell hook, and this TypeScript extension.

The extension writes the marker on `session_start` (once per process) and on
`session_switch` (`/new`, `/resume`, fork, and handoff), when omp changes the
session id in place. It does not remove the marker on shutdown; a marker older
than twelve hours is ignored by the wrapper.

The wrapper detects omp from `OMPCODE=1` or a parent process named `omp`. omp
also exports `CLAUDECODE=1` to its shell, so the wrapper tests omp before
Claude. Without a marker, attribution falls back to a visibly synthetic
harness/directory/day identifier. Explicit CLI `--client`/`--session` and
`TRUFFLEPIG_SESSION` retain precedence over detection and markers.

Concurrent omp sessions in one repository share the per-cwd marker, so the
latest start or switch wins. This matches Kimi and Muse and is a documented
limitation of per-cwd markers.

## Execution

omp invokes `trufflepig-agent` through its shell tool. The wrapper uses the
same [shared runtime and router](README.md#install) as the other harnesses and
supplies the configured spool to the CLI. omp has no sandbox write allowlist
to configure, so no `--runtime-dir` or `--systemd` change is needed for
attribution to work.

## Verification and removal

```sh
python3 -m unittest discover -s plugins/trufflepig-agent/tests -p 'test_*.py'
trufflepig-audit --harness omp --since 1 --json
```

The test suite runs the extension under `bun` when it is on `PATH` and checks
that the wrapper reads the marker it wrote. In a fresh omp session, ask omp to
locate an implementation, then confirm the audit rows carry `harness=omp` and
the session id in the marker. To remove the integration, delete the two
symlinks under `~/.omp/agent`. Shared wrapper commands, runtime configuration,
and any user service may still be used by other harnesses.
