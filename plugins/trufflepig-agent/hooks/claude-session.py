#!/usr/bin/env python3
"""Claude SessionStart/SubagentStart hook for session identity and search guidance."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
EVENTS = ("SessionStart", "SubagentStart")


def subagent_identity_context(payload: dict) -> str:
    session = payload.get("session_id")
    agent = payload.get("agent_id")
    if (
        not isinstance(session, str)
        or not session.strip()
        or not isinstance(agent, str)
        or not agent.strip()
    ):
        return (
            "Trufflepig attribution is unavailable because Claude did not provide both session_id and agent_id. "
            "Before any Trufflepig call, ask the parent for a unique TRUFFLEPIG_SESSION override and prefix each "
            "wrapper command with `TRUFFLEPIG_SESSION='<value>'`. Do not run `trufflepig-agent board hello` or "
            "an identity-sensitive board write with the inherited parent session."
        )
    identity = hashlib.sha256(
        f"{session}\0{agent}".encode("utf-8", "surrogatepass")
    ).hexdigest()[:24]
    override = f"claude-agent-{identity}"
    quoted = shlex.quote(override)
    return (
        "For every Trufflepig command from this subagent, including `board hello`, prefix the command "
        f"with `TRUFFLEPIG_SESSION={quoted}`; for example, "
        f"`TRUFFLEPIG_SESSION={quoted} trufflepig-agent board hello MODEL`. "
        "This stable per-agent override uses Claude's session_id and agent_id and leaves the parent "
        "session environment unchanged."
    )


def search_context(cwd: str, subagent: bool) -> str | None:
    try:
        import trufflepig_checkout as checkout
        import trufflepig_steer as policy
        if policy.steering_mode("claude") == "off":
            return None
        found = checkout.indexed_checkout(Path(cwd).resolve())
        return checkout.session_context(found, subagent) if found else None
    except Exception as error:  # guidance must never prevent the session from starting
        print(f"trufflepig Claude search guidance unavailable: {error}", file=sys.stderr)
        return None


def main() -> int:
    try:
        payload = json.load(sys.stdin)
        event = payload.get("hook_event_name") if isinstance(payload, dict) else None
        if event not in EVENTS:
            return 0
        context = subagent_identity_context(payload) if event == "SubagentStart" else None
        search = search_context(str(payload.get("cwd") or os.getcwd()), event == "SubagentStart")
        if search:
            context = f"{context}\n\n{search}" if context else search
        if context:
            print(json.dumps({"hookSpecificOutput": {"hookEventName": event, "additionalContext": context}}))
        session = payload.get("session_id")
        destination = os.environ.get("CLAUDE_ENV_FILE")
        if event != "SessionStart" or not isinstance(session, str) or not session or not destination:
            return 0
        with Path(destination).open("a", encoding="utf-8") as output:
            output.write(f"\nexport TRUFFLEPIG_CLAUDE_SESSION={shlex.quote(session)}\n")
    except (OSError, ValueError) as error:
        # Attribution must not prevent the user's Claude session from starting.
        print(f"trufflepig Claude attribution unavailable: {error}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
