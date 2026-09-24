#!/usr/bin/env python3
"""Persist Claude's hook-provided session identity in its Bash environment file, and
tell the session how to search when it starts inside an indexed checkout."""
import json
import os
from pathlib import Path
import shlex
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))


def search_context(cwd: str) -> str | None:
    try:
        import trufflepig_checkout as checkout
        import trufflepig_steer as policy
        if policy.mode_for("claude") == "off":
            return None
        found = checkout.indexed_checkout(Path(cwd).resolve())
        return checkout.session_context(found) if found else None
    except Exception as error:  # guidance must never prevent the session from starting
        print(f"trufflepig Claude search guidance unavailable: {error}", file=sys.stderr)
        return None


def main() -> int:
    try:
        payload = json.load(sys.stdin)
        if not isinstance(payload, dict) or payload.get("hook_event_name") != "SessionStart":
            return 0
        context = search_context(str(payload.get("cwd") or os.getcwd()))
        if context:
            print(json.dumps({"hookSpecificOutput": {"hookEventName": "SessionStart",
                                                     "additionalContext": context}}))
        session = payload.get("session_id")
        destination = os.environ.get("CLAUDE_ENV_FILE")
        if not isinstance(session, str) or not session or not destination:
            return 0
        with Path(destination).open("a", encoding="utf-8") as output:
            output.write(f"\nexport TRUFFLEPIG_CLAUDE_SESSION={shlex.quote(session)}\n")
    except (OSError, ValueError) as error:
        # Attribution must not prevent the user's Claude session from starting.
        print(f"trufflepig Claude attribution unavailable: {error}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
