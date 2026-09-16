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
