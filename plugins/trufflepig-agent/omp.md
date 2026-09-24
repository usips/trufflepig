# omp integration

## Installation and discovery

```sh
plugins/trufflepig-agent/install.sh --omp --check "$PWD"
```

`--omp` installs the skill into `skills/trufflepig-code-search` and the
attribution extension into `extensions/trufflepig-session.ts` under the active
agent directory. Resolution follows omp's native configuration:

- `--omp-agent-dir DIR` explicitly selects an installation destination and
  wins over any profile or directory environment variable.
- A named `OMP_PROFILE` (or `PI_PROFILE` when unset) selects
  `~/<PI_CONFIG_DIR>/profiles/<profile>/agent`.
- For the default profile, `PI_CODING_AGENT_DIR` overrides the directory;
  otherwise it is `~/<PI_CONFIG_DIR>/agent`, with `.omp` as the default root.
  Like omp, an absolute `PI_CONFIG_DIR` is still joined under the home
  directory.

An empty or `default` profile selects the default profile. `XDG_CONFIG_HOME`
does not select this directory. For a profile chosen through `omp --profile`,
pass its directory explicitly or set the matching profile environment variable
when installing. See [omp configuration](https://github.com/can1357/oh-my-pi/blob/main/docs/config-usage.md).

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

The extension handles omp's `tool_call` event for `bash` and returns the
original input with `TRUFFLEPIG_OMP_SESSION` added to its environment. It reads
`ctx.sessionManager.getSessionId()` for each call, so `/new`, `/resume`, forks,
and directory changes retain the current session identity. Other tool inputs
and existing shell environment values are preserved. This requires omp's
extension API to support returning revised `tool_call` input.

The wrapper detects omp from `OMPCODE=1` before `CLAUDECODE=1`, which omp also
exports. It reads the injected session identifier independently of cwd; no
per-directory marker is used for omp. Separate sessions do not overwrite each
other's attribution. Without the extension, attribution falls back to a
visibly synthetic harness/directory/day identifier. Explicit CLI `--session`
and `TRUFFLEPIG_SESSION` override the injected session identifier.

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
that searches from the session root, a subdirectory, and a sibling directory
retain the session identity, including after a switch. In a fresh omp session,
ask omp to locate an implementation and check its audit session id. To remove
the integration, delete the two symlinks under the selected agent directory. Shared wrapper commands, runtime configuration,
and any user service may still be used by other harnesses.
