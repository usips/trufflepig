"""Stable per-subagent session identities shared by Claude hooks."""
from __future__ import annotations

import hashlib
from collections.abc import Mapping


def subagent_session(session_id: object, agent_id: object) -> str | None:
    """Return the stable board session token when Claude supplies both IDs."""
    if not isinstance(session_id, str) or not session_id.strip():
        return None
    if not isinstance(agent_id, str) or not agent_id.strip():
        return None
    digest = hashlib.sha256(
        f"{session_id}\0{agent_id}".encode("utf-8", "surrogatepass")
    ).hexdigest()[:24]
    return f"claude-agent-{digest}"


def payload_subagent_session(payload: Mapping[str, object]) -> str | None:
    """Read Claude's session and agent IDs without inventing missing values."""
    return subagent_session(payload.get("session_id"), payload.get("agent_id"))
