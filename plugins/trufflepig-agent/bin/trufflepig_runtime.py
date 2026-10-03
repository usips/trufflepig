"""Shared agent runtime paths and explicit, data-only installation settings."""
from __future__ import annotations

import json
import os
from pathlib import Path


def runtime_config_path() -> Path:
    base = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    return base / "trufflepig" / "agent-runtime.json"


def runtime_settings() -> dict:
    try:
        value = json.loads(runtime_config_path().read_text())
        return value if isinstance(value, dict) else {}
    except (OSError, ValueError):
        return {}


def fallback_root() -> Path:
    configured = runtime_settings().get("runtime_dir")
    if configured:
        return Path(configured)
    base = Path(os.environ.get("TMPDIR") or Path.home() / ".cache" / "codex-tmp")
    return base / f"trufflepig-{os.getuid()}"


def client_environment() -> dict[str, str]:
    env = dict(os.environ)
    spool = runtime_settings().get("spool_dir")
    if spool:
        env.setdefault("TRUFFLEPIG_SPOOL_DIR", spool)
    return env


def steering_mode(harness: str) -> str:
    """Resolve the caller's steering setting with the hook's precedence."""
    modes = {"off", "nudge", "block", "strict"}
    explicit = os.environ.get("TRUFFLEPIG_AGENT_STEER", "").lower()
    if explicit in modes:
        return explicit
    configured = runtime_settings().get("steer")
    value = str(configured.get(harness) or configured.get("default") or "").lower() \
        if isinstance(configured, dict) else ""
    return value if value in modes else "nudge" if harness == "claude" else "block"
