#!/usr/bin/env python3
"""Persist Claude's hook-provided session identity in its Bash environment file."""
import json
import os
from pathlib import Path
import shlex
import sys


def main() -> int:
    try:
        payload = json.load(sys.stdin)
        if not isinstance(payload, dict) or payload.get("hook_event_name") != "SessionStart":
            return 0
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
