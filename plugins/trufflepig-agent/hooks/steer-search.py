#!/usr/bin/env python3
"""PreToolUse/PostToolUse hook: steer shell searches toward trufflepig-agent.

Usage: steer-search.py <claude|kimi|muse>   (hook JSON payload on stdin)

Applies only inside checkouts of registered trufflepig workspace members
(`~/.config/trufflepig/workspaces.toml`, linked worktrees included) or under a
`trufflepig.workspace.toml`. Each shell search is classified (definition, body,
outline, references, regex, concept, files); pipe filters, log/output files,
other revisions, filesystem `find` actions, and paths outside the checkout are
never steered. Modes (TRUFFLEPIG_AGENT_STEER, else `steer.<harness>` in
agent-runtime.json, else claude=nudge, others=block):
  off     do nothing;
  nudge   allow, then add the equivalent trufflepig-agent command as context
          (Claude: PostToolUse additionalContext; others: stdout);
  block   deny until any trufflepig-agent call from this directory in the last
          45 minutes, then allow (the original Kimi/Muse behavior);
  strict  deny definition/body/outline/references searches with the equivalent
          command unless a trufflepig call from this checkout returned no hits
          or failed in the last 10 minutes, or the command carries
          `# tp-fallback: reason`; nudge the remaining classes.
PreToolUse decisions are appended to the agent audit log as verb "hook:steer".
"""
from __future__ import annotations

import json
import os
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import trufflepig_checkout as checkout  # noqa: E402
import trufflepig_classify as classifier  # noqa: E402
import trufflepig_shell as shell  # noqa: E402
import trufflepig_steer as policy  # noqa: E402

SEARCH_TOOLS = re.compile(r"^(grep|glob|ripgrep|rg|find_files|search_files|list_files)$", re.I)
SHELL_TOOLS = re.compile(r"^(bash|shell|run_command|execute|terminal)$", re.I)


def claude_output(event: str, **fields) -> None:
    sys.stdout.write(json.dumps({"hookSpecificOutput": {"hookEventName": event, **fields}}) + "\n")


def message_for(searches: list[classifier.Search], owner: checkout.IndexedRoot, blocked: bool,
                mode: str = "strict") -> str:
    lines = [f"{owner.member} is indexed by trufflepig; "
             + ("run the equivalent instead:" if blocked else "next time use the one-call equivalent:")]
    for search in searches[:3]:
        lines.append(f"- {search.kind} search `{search.pattern[:80]}` -> {search.hint}")
    if blocked and mode == "block":
        lines.append("Ordinary search is allowed again after one trufflepig-agent call from this directory.")
    elif blocked:
        lines.append("If trufflepig returns nothing useful, retry the grep with a trailing "
                     "`# tp-fallback: <reason>` comment.")
    return "\n".join(lines)


def main() -> int:
    harness = sys.argv[1] if len(sys.argv) > 1 else "unknown"
    mode = policy.mode_for(harness)
    if mode == "off":
        return 0
    try:
        payload = json.load(sys.stdin)
    except ValueError:
        return 0
    if not isinstance(payload, dict):
        return 0
    event = str(payload.get("hook_event_name") or "PreToolUse")
    tool = str(payload.get("tool_name") or payload.get("tool") or payload.get("name") or "")
    tool_input = payload.get("tool_input") or payload.get("input") or payload.get("arguments") or {}
    if not isinstance(tool_input, dict):
        tool_input = {}
    cwd = Path(payload.get("cwd") or os.getcwd()).resolve()

    if SEARCH_TOOLS.match(tool):
        pattern = str(tool_input.get("pattern") or tool_input.get("query") or "")
        command = f"rg -n {classifier.quote(pattern)} {classifier.quote(str(tool_input.get('path') or '.'))}"
    elif SHELL_TOOLS.match(tool):
        command = tool_input.get("command") or tool_input.get("cmd") or ""
        command = " ".join(map(str, command)) if isinstance(command, list) else str(command)
    else:
        return 0
    if not classifier.MAYBE_SEARCH.search(command):
        return 0
    owner = checkout.indexed_checkout(shell.final_directory(command, cwd))
    if owner is None:
        return 0
    searches = classifier.classify(command, cwd, owner.checkout)
    if not searches:
        return 0

    session = str(payload.get("session_id") or payload.get("sessionId") or "")
    agent = str(payload.get("agent_id") or payload.get("agentId") or "")
    strong = [s for s in searches if s.kind in policy.STRONG_CLASSES]
    fallback = None
    if policy.FALLBACK_MARKER.search(command):
        fallback = "explicit tp-fallback"
    if mode == "block":
        decision = "allow" if fallback or policy.used_recently(harness, str(cwd)) else "block"
    elif mode == "strict" and strong:
        fallback = fallback or policy.recent_fallback_reason(harness, owner.checkout)
        decision = "allow" if fallback else "block"
    else:
        decision = "nudge"

    if event == "PostToolUse":
        # Nudges are delivered after the search ran, where Claude accepts added context.
        if harness == "claude" and decision == "nudge" and not fallback and \
                policy.should_nudge(f"{session}:{agent}:{cwd}", searches[0].kind):
            claude_output("PostToolUse", additionalContext=message_for(searches, owner, False))
        return 0

    policy.log({
        "ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "harness": harness,
        "session": session or f"{harness}-{checkout.cwd_key(str(cwd))}-{time.strftime('%Y%m%d')}",
        "agent": agent,
        "agent_type": str(payload.get("agent_type") or ""),
        "cwd": str(cwd),
        "verb": "hook:steer",
        "args": [searches[0].program, searches[0].pattern[:120]],
        "options": {"mode": mode},
        "classes": [s.kind for s in searches],
        "hint": searches[0].hint[:200],
        "fallback": fallback or "",
        "exit_code": 2 if decision == "block" else 0,
        "elapsed_ms": 0,
        "status": decision,
        "error": "",
        "stderr": "",
        "hits": None,
        "truncated": None,
        "has_next": False,
        "stdout_bytes": 0,
        "coverage": {},
        "repeat_count": 0,
        "signals": ["steer_block"] if decision == "block" else (["steer_nudge"] if decision == "nudge" else []),
    })
    if decision == "block":
        blocked = strong if mode == "strict" else searches
        message = message_for(blocked, owner, True, mode)
        if harness == "claude":
            claude_output("PreToolUse", permissionDecision="deny", permissionDecisionReason=message)
            return 0
        sys.stderr.write(message + "\n")
        return 2
    if decision == "nudge" and harness != "claude":
        sys.stdout.write(message_for(searches, owner, False) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
