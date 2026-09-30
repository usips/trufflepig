"""Steering policy: per-harness mode, fallback evidence, tip cadence, audit records.

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
TIP_MARKER_SECONDS = 24 * 60 * 60
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


def first_tip(key: str, kind: str) -> bool:
    """Whether this agent (`key`: session, agent, cwd) gets its first tip of `kind`; the
    full tip comes once, later ones use the one-line form. Creating the marker is the
    atomic claim, so concurrent hooks agree on which one is first."""
    markers = state_dir() / "steer-nudge"
    try:
        markers.mkdir(parents=True, exist_ok=True)
        with open(markers / f"{cwd_key(key)}-{kind}", "x"):
            pass
    except FileExistsError:
        return False
    except OSError:
        return True
    prune_tip_markers(markers)
    return True


def prune_tip_markers(markers: Path) -> None:
    """At most once per `TIP_MARKER_SECONDS`, remove markers older than that."""
    stamp = markers / ".pruned"
    now = time.time()
    try:
        if now - stamp.stat().st_mtime < TIP_MARKER_SECONDS:
            return
    except OSError:
        pass
    try:
        stamp.touch()
        candidates = list(markers.iterdir())
    except OSError:
        return
    for marker in candidates:
        try:
            if marker != stamp and now - marker.stat().st_mtime > TIP_MARKER_SECONDS:
                marker.unlink()
        except OSError:
            continue  # a concurrent hook pruned it first


def log(record: dict) -> None:
    for target in audit_dirs():
        try:
            target.mkdir(parents=True, exist_ok=True)
            with (target / f"{record['harness']}.jsonl").open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(record, separators=(",", ":")) + "\n")
            return
        except OSError:
            continue
