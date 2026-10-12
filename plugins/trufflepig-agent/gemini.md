# Gemini CLI integration

Install the shared search and plan board skills and wrapper:

```sh
plugins/trufflepig-agent/install.sh --gemini --check "$PWD"
gemini skills list
```

The installer links `trufflepig-code-search` and `trufflepig-plan-board` into
`~/.gemini/skills`. When `GEMINI_CLI_HOME` is set, it uses
`$GEMINI_CLI_HOME/.gemini/skills`, following Gemini CLI's user-home override.
Gemini also discovers `~/.agents/skills`, which may already expose skills from
a Codex install. The installer preserves unmanaged destinations and configures
the shared disk-backed runtime. Router setup and GPU build options are in the
[installation guide](README.md#install).

Gemini discovers user skills across projects. Run `gemini skills list` or
`/skills list` in an interactive session to verify discovery. After an install
while Gemini is already running, use `/skills reload` (or `/skills refresh`)
to rescan; opening a new session also loads the current skill files.
Automatic skill selection remains Gemini's decision.

Gemini shell calls set `GEMINI_CLI=1`, which the wrapper uses to identify the
harness. `GEMINI_SESSION_ID` is available to Gemini hooks but is not forwarded
to ordinary shell calls, so those audit records use the harness/cwd/day
fallback instead of claiming a unique conversation ID. A launcher that needs
exact attribution can set `TRUFFLEPIG_AGENT_HARNESS=gemini` and
`TRUFFLEPIG_SESSION` explicitly.

If Gemini's shell does not include the user bin directory in `PATH`, invoke the
wrapper by its installed path (the default is `~/.local/bin/trufflepig-agent`):

```sh
"$HOME/.local/bin/trufflepig-agent" search 'sym:main'
python3 plugins/trufflepig-agent/scripts/check_agent.py --wrapper "$HOME/.local/bin/trufflepig-agent" "$PWD"
trufflepig-audit --harness gemini --since 1 --json
```

The installer does not change Gemini permissions. The workspace sandbox must
allow the wrapper and shared runtime directory; see
[runtime recovery](README.md#runtime-recovery-and-upgrades). Remove the two
installed skill links to uninstall these native discovery entries. A shared
`.agents/skills` link may continue to expose them.

Harness contracts: [Agent Skills](https://geminicli.com/docs/cli/using-agent-skills/),
[shell tool](https://geminicli.com/docs/tools/shell/).
