# Claude Code integration

## Installation and discovery

```sh
plugins/trufflepig-agent/install.sh --claude --check "$PWD"
```

Alternatively, install the Claude plugin from this repository's marketplace
(`.claude-plugin/marketplace.json`); it carries both skills and the hooks in
`hooks/hooks.json`, run from `${CLAUDE_PLUGIN_ROOT}`:

```sh
claude plugin marketplace add ~/Source/trufflepig
claude plugin install trufflepig-agent@trufflepig
plugins/trufflepig-agent/install.sh --commands --claude   # wrapper commands, permission
```

With the plugin enabled, `install.sh --claude` links neither the personal skills
nor the hooks, so nothing runs twice; it still adds the wrapper permission and
runtime access. `claude --plugin-dir plugins/trufflepig-agent` loads a checkout
for a single session. `claude plugin validate --strict` checks both manifests.

`--claude` installs `trufflepig-code-search` and `trufflepig-plan-board` at
`$CLAUDE_CONFIG_DIR/skills/`, defaulting to
`~/.claude/skills/`. Claude follows each symlink to this
checkout. Personal skills apply across projects; a project-only manual install
uses `.claude/skills`, not `.agents/skills`. Keep the checkout available because
the skills and commands are linked to it.

Claude can select this skill automatically from its description. Invoke
`/trufflepig-code-search` to request it explicitly. The shared frontmatter does
not disable model invocation, hide the command, fork a subagent, inject shell
commands, or pre-approve tools. Codex's `agents/openai.yaml` is not Claude's
invocation policy. See [Claude skills](https://code.claude.com/docs/en/skills).

Use a new Claude session after installation so the session hook runs. Existing
sessions may discover the skill through file watching but lack session exports.
Safe mode, disabled hooks, setting-source restrictions, managed customization
policies, and `skillOverrides` can change availability; the installer does not
override those controls. Check `/skills` and `/hooks` in the target session.

## Session attribution

The installer merges one synchronous, unfiltered `SessionStart` command into
Claude's user `settings.json`, preserving unrelated hooks and settings. It runs
on startup, resume, clear, and compaction. The hook reads `session_id` from JSON
stdin and appends a shell-quoted `TRUFFLEPIG_CLAUDE_SESSION` export to
`CLAUDE_ENV_FILE`. Each session owns its environment file, so concurrent sessions
in one repository do not overwrite a shared per-directory marker. Repeated
starts append the current identity; later Bash calls use the latest export.

This uses the documented [hook environment interface](https://code.claude.com/docs/en/hooks#persist-environment-variables).
`${CLAUDE_SESSION_ID}` in skill text is a substitution, not a guaranteed Bash
environment variable. The wrapper detects the documented `CLAUDECODE=1` or
`CLAUDE_CODE_CHILD_SESSION=1` markers and reads the integration's exported session.
See [Claude environment variables](https://code.claude.com/docs/en/env-vars).
Identity can come from explicit CLI `--session` or `TRUFFLEPIG_SESSION`, the
native harness session, or `AGENT_SESSION_ID`. If none is present, the wrapper
preserves the missing identity; it does not synthesize a harness/directory/day
token. Missing environment files or hook I/O errors do not block startup.

## Search guidance and steering

Claude Code on this platform has no Grep or Glob tools: it runs `grep` and `find`
through Bash (shadowed by bundled ugrep/bfs). Guidance therefore targets shell
searches, not tool names.

When a session starts inside an indexed checkout, the `SessionStart` hook also
returns `additionalContext`: the member and workspace, replacement commands for
definition, body, reference, outline, file, and line-range (`sed -n`) reads,
exact `next: more SET@OFFSET` → `trufflepig-agent more SET@OFFSET` and
`next: show read:H@B` → `trufflepig-agent show read:H@B` mappings, and separate
optional `hint:` advice. It explains whole-query quoting for negative filters,
`file:` prefix/component matching, and linked-worktree fallback: `differs` may
carry parent coordinates, `show` re-extracts changed bytes when possible, and
`verified`/`source` must be checked before claiming current bytes. Incomplete or
truncated coverage cannot establish absence. It also reminds the main agent to
brief subagents the same way.
`SessionStart` context does not reach subagents, so the same script, registered
for `SubagentStart`, returns that guidance (without the briefing reminder) as the
subagent's `additionalContext`. Claude's `SubagentStart` payload carries the parent
`session_id` and a unique `agent_id`. Their SHA-256 with a NUL separator yields a
stable `claude-agent-<24 hex>` token, shared by tool hooks and that subagent's stop
hook. It does not replace the parent session export. Workers use the worker address
and role from the lead's brief; `agent_type` does not select a Board role. If either
ID is missing, the hook requires a unique explicit `TRUFFLEPIG_SESSION` and never
uses the parent's identity for a subagent write.
Outside indexed checkouts, or with steering `off`, code-search guidance is omitted.
Subagent identity guidance is still returned whenever Claude supplies both IDs. If either
ID is missing, the hook marks attribution unavailable and tells the child to obtain a unique
`TRUFFLEPIG_SESSION` from its parent before making board calls; it must not reuse the parent's
identity for `board hello` or other identity-sensitive writes.

The installer merges search steering on `PreToolUse`/`PostToolUse` Bash and
`PreToolUse` Agent hooks, then adds `claude-hooks.py` for Bash identity rewriting,
all-tool post-use notices, and session/subagent cleanup. Identity rewriting runs
after Bash search steering, which retains the original command for classification.
Tool hooks fire inside subagents too; their payloads carry `agent_id` and
`agent_type`, which audit records retain. Deny reasons and `additionalContext`
reach the model; plain `PreToolUse` stdout does not, so nudges wait for `PostToolUse`.
Claude Code documents `SubagentStart` and `PreToolUse` `additionalContext` as
model context too; the `Agent` brief check and subagent guidance rely on that.
In the default `nudge` mode, `PreToolUse` records the classified search and
allows it; `PostToolUse` then adds the equivalent Trufflepig command as context:
in full for the first search of each class per agent, as one line for every
later one. A `trufflepig-agent` call piped into another program (`| head`) or
chained with others (`;`, `&&`; a leading `cd DIR &&` is fine) gets a
`PostToolUse` tip that the footer and exit status were lost. An `Agent` call
whose brief tells the subagent to grep code (`just grep for`, `use rg`; quoted,
negated, and log-search mentions are ignored) gets lead-facing `PreToolUse` `additionalContext`; the brief itself is
never rewritten. In `strict` mode
`PreToolUse` returns `permissionDecision: "deny"` with the equivalent command for
definition, body, outline, and reference searches. See
[Other harness hooks](README.md#other-harness-hooks) for classes, fallbacks, and
`install.sh --steer MODE`.

Automatic identity rewriting stays disabled by default. The native Claude Code
2.1.296 fixture confirmed that the hook's `updatedInput` reaches normal permission
evaluation, then showed that `Bash(trufflepig-agent *)` does not authorize the
rewritten command, which starts with `TRUFFLEPIG_SESSION=...`; Claude denied the
probe before the wrapper ran. No permission decision is bypassed. Set
`TRUFFLEPIG_CLAUDE_IDENTITY_REWRITE=1` only when the active permission rules
already authorize the rewritten command. The hook does not change those rules.

With opt-in enabled, the PreToolUse hook prefixes direct `trufflepig-agent` and
`trufflepig` Bash calls with the stable subagent token. Explicit
`TRUFFLEPIG_SESSION=` and `--session` overrides remain intact; all other tool
input fields are preserved. The hook returns `updatedInput` without
`permissionDecision`. The [SDK hook contract](https://code.claude.com/docs/en/agent-sdk/hooks#modified-input-not-applied)
specifies that omitting `permissionDecision` preserves normal permission
evaluation. Manual `TRUFFLEPIG_SESSION=...` prefixes use that same permission
flow and may also prompt under the existing allow rule.

After a successful or failed tool call, the hook checks pending Board notices at
most once per 60 seconds per actor. It emits only compact entry/task IDs, model,
and age; free-form Board text is omitted. Shared atomic state lives under
`$XDG_STATE_HOME/trufflepig/claude-notices`, defaulting to
`~/.local/state/trufflepig/claude-notices`. An unavailable Board fails open.
`SessionEnd` sends `board bye` for the top-level session; `SubagentStop` sends it
for only that subagent's derived identity. Both calls are bounded to one second
and return no model context.

## Execution and sandbox

Claude invokes `trufflepig-agent` through Bash. The wrapper uses the same
[shared runtime and router](README.md#install) as Codex. The installer adds
`Bash(trufflepig-agent *)` to `permissions.allow` so wrapper calls never prompt, and
appends only the runtime directory to `sandbox.filesystem.allowWrite`. It neither
enables nor disables sandboxing, changes permission modes, excludes commands from
isolation, nor grants blanket Unix socket access.

Claude documents [specific sandbox write paths](https://code.claude.com/docs/en/sandboxing#configuration)
for tools that need state outside the project. When socket access is unavailable,
the wrapper can reach the router through the configured disk-backed spool. The
router must run with that same spool setting. Use `--systemd` during initial
router setup or an intentional upgrade; adding Claude to an existing configured
runtime does not require restarting the service.

## Verification and removal

```sh
python3 -m unittest discover -s plugins/trufflepig-agent/tests -p 'test_*.py'
trufflepig-audit --harness claude --since 1 --json
trufflepig-audit --harness claude --since 72 --adoption
```

In a fresh session, ask Claude to locate an implementation, then inspect the
trace for skill selection and `trufflepig-agent search` followed by `show` or
`ctx`. `/trufflepig-code-search` tests explicit activation. Discovery and hook
initialization can be checked without a model turn using Claude's SDK control
interface; neither proves that a model will choose the skill on every prompt.
Compare the audit session with Claude's actual session ID, including separate
sessions in the same working directory and a resumed session.

The isolated Claude Code 2.1.296 check confirmed ordinary permission evaluation
and showed that the existing allow rule rejects the rewritten command. Keep the
default disabled unless a future permission policy intentionally authorizes that
command form; do not use hook output to bypass permission checks.

To remove the integration, remove both personal skill symlinks, the
`SessionStart` and `SubagentStart` entries invoking `trufflepig-claude-session`,
the `PreToolUse` and `PostToolUse` entries invoking `trufflepig-agent-steer claude`,
the `claude-hooks.py` entries for `PreToolUse`, post-use events, `SessionEnd`, and
`SubagentStop`, and the
`Bash(trufflepig-agent *)` permission. `install.sh --claude --steer off` disables
steering without editing settings. Remove the runtime
allowWrite entry if no other Claude integration needs it. Shared wrapper commands,
runtime configuration, and the service may still be used by other harnesses.
