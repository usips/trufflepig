"""Steering policy: per-harness mode, fallback evidence, nudge rate limits, audit records.

Shared by `steer-search.py` (PreToolUse/PostToolUse) and `claude-session.py`.
"""
from __future__ import annotations

import json
import os
import re
import time
from pathlib import Path

from trufflepig_checkout import config_dir, cwd_key, state_dir

# Classes where the translated command answers the same question in one call.
STRONG_CLASSES = {"definition", "body", "outline", "references"}
FALLBACK_MARKER = re.compile(r"#\s*tp-fallback\b")
FALLBACK_WINDOW_SECONDS = 10 * 60
UNLOCK_WINDOW_SECONDS = 45 * 60
NUDGE_INTERVAL_SECONDS = 90
MODES = ("off", "nudge", "block", "strict")
DEFAULT_MODES = {"claude": "nudge"}


def mode_for(harness: str) -> str:
    explicit = os.environ.get("TRUFFLEPIG_AGENT_STEER", "").lower()
    if explicit in MODES:
        return explicit
    try:
        configured = json.loads((config_dir() / "agent-runtime.json").read_text()).get("steer") or {}
    except (OSError, ValueError, AttributeError):
        configured = {}
    value = str(configured.get(harness) or configured.get("default") or "").lower() \
        if isinstance(configured, dict) else ""
    return value if value in MODES else DEFAULT_MODES.get(harness, "block")


def audit_dirs() -> list[Path]:
    primary = Path(os.environ.get("TRUFFLEPIG_AGENT_LOG_DIR") or state_dir() / "agent-audit")
    directories = [primary]
    try:
        runtime = json.loads((config_dir() / "agent-runtime.json").read_text()).get("runtime_dir")
        if runtime:
            directories.append(Path(runtime) / "agent-audit")
    except (OSError, ValueError, AttributeError):
        pass
    return directories


def recent_records(harness: str, within: float, tail_bytes: int = 262144) -> list[dict]:
    """Audit records for `harness` from the last `within` seconds (tail of each log)."""
    cutoff = time.time() - within
    records: list[dict] = []
    for directory in audit_dirs():
        path = directory / f"{harness}.jsonl"
        try:
            with path.open("rb") as handle:
                handle.seek(0, os.SEEK_END)
                handle.seek(max(0, handle.tell() - tail_bytes))
                lines = handle.read().splitlines()[1:] if handle.tell() > tail_bytes else handle.read().splitlines()
        except OSError:
            continue
        for line in lines:
            try:
                record = json.loads(line)
                stamp = time.mktime(time.strptime(record["ts"][:19], "%Y-%m-%dT%H:%M:%S"))
            except (ValueError, KeyError, TypeError):
                continue
            if stamp >= cutoff:
                records.append(record)
    return records


def recent_fallback_reason(harness: str, checkout: Path) -> str | None:
    """Why ordinary search is justified: a recent trufflepig call from this checkout
    that returned nothing or failed."""
    for record in reversed(recent_records(harness, FALLBACK_WINDOW_SECONDS)):
        if str(record.get("verb", "")).startswith("hook:"):
            continue
        cwd = Path(str(record.get("cwd") or "/"))
        if cwd != checkout and checkout not in cwd.parents:
            continue
        signals = set(record.get("signals") or [])
        if "no_hits" in signals:
            return "trufflepig returned no hits"
        if "error" in signals:
            return "trufflepig failed"
    return None


def used_recently(harness: str, cwd: str) -> bool:
    """Legacy `block` policy: any trufflepig-agent call from this directory recently."""
    recent = state_dir() / "agent-recent" / harness
    if not recent.is_dir():
        return False
    now = time.time()
    for path in recent.glob(f"{cwd_key(cwd)}-*"):
        try:
            if now - path.stat().st_mtime <= UNLOCK_WINDOW_SECONDS:
                return True
        except OSError:
            continue
    return False


def should_nudge(key: str, kind: str) -> bool:
    """Rate-limit repeated tips of the same class to one per interval per agent."""
    marker = state_dir() / "steer-nudge" / f"{cwd_key(key)}-{kind}"
    try:
        if time.time() - marker.stat().st_mtime < NUDGE_INTERVAL_SECONDS:
            return False
    except OSError:
        pass
    try:
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.touch()
    except OSError:
        pass
    return True


def log(record: dict) -> None:
    for target in audit_dirs():
        try:
            target.mkdir(parents=True, exist_ok=True)
            with (target / f"{record['harness']}.jsonl").open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(record, separators=(",", ":")) + "\n")
            return
        except OSError:
            continue
