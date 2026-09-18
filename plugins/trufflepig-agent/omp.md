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

omp auto-discovers both: it scans `~/.omp/agent/skills` and
`~/.omp/agent/extensions`, loading top-level `.ts`/`.js` extension files without
a manifest. Both are symlinks into this checkout, so keep the checkout
available. Start a new omp session after installation so the skill is
discovered and the extension loads.

omp selects the skill from its description; availability does not guarantee
selection on every prompt. Running `--codex` and `--omp` together leaves the
skill at two paths (`~/.agents/skills` and `~/.omp/agent/skills`), but omp
deduplicates skills by name, so there is no functional conflict.

## Session attribution

The extension subscribes to omp's `session_start` event, which fires in every
mode (print, interactive, ACP, task). On start it writes the session id to a
per-cwd marker:

```
$XDG_STATE_HOME/trufflepig/agent-sessions/omp/<sha256(cwd) first 12 hex>
```

Default `XDG_STATE_HOME` is `~/.local/state`. omp exports no per-session
environment variable to shell children, so the marker file is the identity
channel (the same pattern Kimi/Muse use).

The marker key is `sha256(cwd)` truncated to twelve hex characters. omp runs
on Bun/Node, whose OpenSSL exposes only full-length `blake2s`, whose first six
bytes differ from Python's `blake2s(digest_size=6)`; the key must be
reproducible in both the TypeScript extension and the Python wrapper, so it
uses SHA-256, which is byte-identical across Python, Bun, and Node. Other
harnesses keep their blake2s keys unchanged.

The wrapper detects omp from `OMPCODE=1` or a parent process named `omp`,
then reads the marker for the working directory. A marker older than twelve
hours is ignored. Without a marker, attribution falls back to a visibly
synthetic harness/directory/day identifier. Explicit CLI `--client`/`--session`
and `TRUFFLEPIG_SESSION` retain precedence over detection and markers.

Concurrent omp sessions in one repository share the per-cwd marker, so the
latest `session_start` wins; this matches the Kimi/Muse behavior and is why
per-cwd (not per-session) markers are a documented limitation, not a bug.

## Execution

omp invokes `trufflepig-agent` through its shell tool. The wrapper uses the
same [shared runtime and router](README.md#install) as the other harnesses and
supplies the configured spool to the CLI. There is no sandbox to configure:
omp does not impose a filesystem-write allowlist, so no `--systemd` or
runtime-directory change is needed for attribution to work.

## Verification and removal

```sh
python3 -m unittest discover -s plugins/trufflepig-agent/tests -p 'test_*.py'
trufflepig-audit --harness omp --since 1 --json
```

In a fresh omp session, ask omp to locate an implementation, then inspect the
audit trace for `harness=omp` and the session id matching the marker. Compare
the audit session with the marker's contents. To remove the integration,
delete the two symlinks under `~/.omp/agent`. Shared wrapper commands, runtime
configuration, and any user service may still be used by other harnesses.
