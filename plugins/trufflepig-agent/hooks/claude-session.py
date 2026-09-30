#!/usr/bin/env python3
"""Claude SessionStart/SubagentStart hook. SessionStart persists the hook-provided
session identity in the Bash environment file; both tell the (sub)agent how to
search when it starts inside an indexed checkout. Subagents never see SessionStart
context or CLAUDE.md, so SubagentStart repeats the guidance for them."""
import json
import os
from pathlib import Path
import shlex
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
EVENTS = ("SessionStart", "SubagentStart")


def search_context(cwd: str, subagent: bool) -> str | None:
    try:
        import trufflepig_checkout as checkout
        import trufflepig_steer as policy
        if policy.mode_for("claude") == "off":
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
        context = search_context(str(payload.get("cwd") or os.getcwd()), event == "SubagentStart")
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
