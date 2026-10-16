# OpenCode integration

Install the shared search and plan board skills, the session plugin, and wrapper:

```sh
plugins/trufflepig-agent/install.sh --opencode --check "$PWD"
opencode debug skill
```

The installer links `trufflepig-code-search` and `trufflepig-plan-board` into
`$XDG_CONFIG_HOME/opencode/skills` (default `~/.config/opencode/skills`) and
[`opencode/session.ts`](opencode/session.ts) into the sibling `plugins` directory
as `trufflepig-session.ts`. It preserves unmanaged destinations and configures
the shared disk-backed runtime. Router setup and GPU build options are in the
[installation guide](README.md#install).

OpenCode also scans `~/.claude/skills` and `~/.agents/skills`, so a Claude or
Codex install may already expose these skills. Duplicate names load once, with
a `duplicate skill name` log warning; every link here resolves to the same
checkout. The model loads a skill through OpenCode's `skill` tool; selection
remains its decision. OpenCode loads skills and plugins at startup, so restart
it after installing. `opencode debug skill` lists the discovered locations.

OpenCode exports `OPENCODE=1` to its shell tool, which the wrapper and CLI use
to identify the harness ahead of `CLAUDECODE`; an `opencode` ancestor process is
the fallback. The shell tool does not export the session ID. The plugin's
`shell.env` hook sets `TRUFFLEPIG_OPENCODE_SESSION` for each agent shell call,
including subagent sessions, which have their own IDs. Terminal panes have no
session and stay unattributed. Without the plugin, set `TRUFFLEPIG_SESSION`
explicitly when exact attribution matters.

Inside OpenCode, run the wrapper smoke check to verify its actual permissions:

```sh
python3 plugins/trufflepig-agent/scripts/check_agent.py "$PWD"
trufflepig-audit --harness opencode --since 1 --json
```

The installer does not change `opencode.json`. Its `permission.bash` and
`permission.skill` rules must allow `trufflepig-agent` and both skills; see
[runtime recovery](README.md#runtime-recovery-and-upgrades) for runtime and
spool access. OpenCode installs no steering hook. Remove the two skill links and the plugin link to uninstall;
shared `.claude` or `.agents` entries may still expose the skills.

Harness contracts: [Agent Skills](https://opencode.ai/docs/skills/),
[plugins](https://opencode.ai/docs/plugins/).
