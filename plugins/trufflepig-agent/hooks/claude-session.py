#!/usr/bin/env python3
"""Claude SessionStart/SubagentStart hook for session identity and search guidance."""
import json
import os
from pathlib import Path
import shlex
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import trufflepig_identity as identity
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
            "Before any `board` command, ask the parent for a unique session value and pass it as "
            "`trufflepig-agent --session VALUE board ...`. Do not run `board hello` or an identity-sensitive "
            "board write with the inherited parent session. Searches need no session."
        )
    override = identity.subagent_session(session, agent)
    if override is None:
        return "Trufflepig attribution is unavailable."
    automatic = identity.identity_rewrite_enabled()
    return (
        f"Your Trufflepig Board session is `{override}`. Pass it right after `trufflepig-agent` on every "
        f"`board` command, e.g. `trufflepig-agent --session {override} board hello MODEL`"
        + ("; direct calls also receive it automatically" if automatic else "")
        + ". Searches need no prefix. "
        "Follow the worker recipe in the lead's brief and use the handle, address, and role the lead registered. "
        "Claude's agent_type is not a Board role, so do not infer or register a role from it. "
        "When the brief provides a worker address, include it in `board hello` as "
        "`--worker TASK/HANDLE` (e.g. P7.3/parser)."
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
