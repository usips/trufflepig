"""Shared agent runtime paths and explicit, data-only installation settings."""
from __future__ import annotations

import hashlib
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

def omp_marker_key(cwd: str) -> str:
    """Marker file name for an omp session, keyed by working directory.

    Uses SHA-256 rather than the wrapper's blake2s because the omp extension
    runs on Bun/Node, whose OpenSSL exposes only full-length blake2s (whose
    first six bytes differ from Python's digest_size=6). SHA-256 truncation is
    byte-identical across all three runtimes.
    """
    return hashlib.sha256(cwd.encode()).hexdigest()[:12]


def client_environment() -> dict[str, str]:
    env = dict(os.environ)
    spool = runtime_settings().get("spool_dir")
    if spool:
        env.setdefault("TRUFFLEPIG_SPOOL_DIR", spool)
    return env
