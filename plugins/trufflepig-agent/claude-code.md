# Claude Code integration

## Installation and discovery

```sh
plugins/trufflepig-agent/install.sh --claude --check "$PWD"
```

Alternatively, install the Claude plugin from this repository's marketplace
(`.claude-plugin/marketplace.json`); it carries the skill and the hooks in
`hooks/hooks.json`, run from `${CLAUDE_PLUGIN_ROOT}`:

```sh
claude plugin marketplace add ~/Source/trufflepig
claude plugin install trufflepig-agent@trufflepig
plugins/trufflepig-agent/install.sh --commands --claude   # wrapper commands, permission
```

With the plugin enabled, `install.sh --claude` links neither the personal skill
nor the hooks, so nothing runs twice; it still adds the wrapper permission and
runtime access. `claude --plugin-dir plugins/trufflepig-agent` loads a checkout
for a single session. `claude plugin validate --strict` checks both manifests.

`--claude` installs the shared skill at
`$CLAUDE_CONFIG_DIR/skills/trufflepig-code-search`, defaulting to
`~/.claude/skills/trufflepig-code-search`. Claude follows the symlink to this
checkout. Personal skills apply across projects; a project-only manual install
uses `.claude/skills`, not `.agents/skills`. Keep the checkout available because
both the skill and commands are linked to it.

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
No SessionEnd cleanup or model-context output is required.

This uses the documented [hook environment interface](https://code.claude.com/docs/en/hooks#persist-environment-variables).
`${CLAUDE_SESSION_ID}` in skill text is a substitution, not a guaranteed Bash
environment variable. The wrapper detects the documented `CLAUDECODE=1` or
`CLAUDE_CODE_CHILD_SESSION=1` markers and reads the integration's exported session.
See [Claude environment variables](https://code.claude.com/docs/en/env-vars).
Explicit CLI client/session options and Trufflepig environment overrides retain
precedence. Without the session hook, attribution falls back to a visibly
synthetic harness/directory/day identifier; it does not pretend to be a Claude
conversation ID. Missing environment files or hook I/O errors do not block startup.

## Search guidance and steering

Claude Code on this platform has no Grep or Glob tools: it runs `grep` and `find`
through Bash (shadowed by bundled ugrep/bfs). Guidance therefore targets shell
searches, not tool names.

When a session starts inside a checkout of a registered workspace member, the
`SessionStart` hook also returns `additionalContext`: the member and workspace,
the five commands that replace definition, body, reference, outline, and file
greps, and a reminder to brief subagents the same way. Outside indexed checkouts
it prints nothing.

The installer merges `PreToolUse` and `PostToolUse` hooks with matcher `Bash`
that run `trufflepig-agent-steer claude`. Tool hooks fire inside subagents too, so
Explore and custom agents receive the same guidance as the main conversation;
their payloads carry `agent_id` and `agent_type`, which the audit records keep.
Deny reasons and `PostToolUse`/`SessionStart` `additionalContext` reach the model;
plain `PreToolUse` stdout does not, so nudges wait for `PostToolUse`.
In the default `nudge` mode, `PreToolUse` records the classified search and
allows it; `PostToolUse` then adds the equivalent Trufflepig command as context,
rate limited to one tip per class per agent every 90 seconds. In `strict` mode
`PreToolUse` returns `permissionDecision: "deny"` with the equivalent command for
definition, body, outline, and reference searches. See
[Other harness hooks](README.md#other-harness-hooks) for classes, fallbacks, and
`install.sh --steer MODE`.

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

To remove the integration, remove only the personal skill symlink, the
`SessionStart` entry invoking `trufflepig-claude-session`, the `PreToolUse` and
`PostToolUse` entries invoking `trufflepig-agent-steer claude`, and the
`Bash(trufflepig-agent *)` permission. `install.sh --claude --steer off` disables
steering without editing settings. Remove the runtime
allowWrite entry if no other Claude integration needs it. Shared wrapper commands,
runtime configuration, and the service may still be used by other harnesses.
