# Grok Build integration

Install the shared search skill and wrapper:

```sh
plugins/trufflepig-agent/install.sh --grok --check "$PWD"
grok inspect --json
```

The installer links `trufflepig-code-search` into `~/.grok/skills`, or
`$GROK_HOME/skills` when configured. It preserves unmanaged destinations and
reuses the shared disk-backed runtime. The router setup and GPU build options
live in the [installation guide](README.md#install).

Grok discovers the skill automatically and offers `/trufflepig-code-search`.
Automatic selection remains the model's decision. Open a new session after
installation and use `grok inspect --json` to verify discovery. Grok also reads
`~/.agents/skills` and Claude skills, deduplicating by name; an existing Codex
installation may already expose this skill.

The wrapper recognizes Grok through `GROK_SESSION_ID` when supplied or a Grok
ancestor process. When no session identifier is available, audit records use
the documented harness/cwd/day fallback, not a claimed unique conversation ID.
Set `TRUFFLEPIG_AGENT_HARNESS=grok` and `TRUFFLEPIG_SESSION` explicitly when an
external launcher hides process ancestry or needs exact conversation attribution.
No Grok session environment export or hook is assumed.

Inside Grok, run the wrapper smoke check to verify its actual permissions:

```sh
python3 plugins/trufflepig-agent/scripts/check_agent.py "$PWD"
trufflepig-audit --harness grok --since 1 --json
```

The installer does not change Grok permissions. A restricted sandbox must permit
the wrapper and the shared runtime directory; see [runtime recovery](README.md#runtime-recovery-and-upgrades).
Remove the installed skill symlink to uninstall the native discovery entry.
Shared `.agents` or Claude entries may still expose the skill.

Harness contracts: [skills and compatibility](https://docs.x.ai/build/features/skills-plugins-marketplaces),
[CLI inspection](https://docs.x.ai/build/cli/reference).
