"""Share bounded Board notice polling and dedupe state between wrappers and hooks.

Callers pass ``BoardReply.result.data`` from a successful ``board pending --json`` response.
``PendingReply`` has ``notices``, ``after``, ``through``, ``server_now``, ``generation`` (opaque lowercase 16-hex), ``omitted``.
Each notice has ``seq``, ``kind``, ``reason``, and exactly one ``entry`` or ``event`` payload.
Projections read only typed IDs, task refs, kind, model, and age; Board text and reason are omitted.
State lives in ``notices.json`` under the caller's private state directory.
Its notifier watermark is separate from the Board inbox cursor and keyed by full actor identity."""
from __future__ import annotations

from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import re
import stat
import tempfile
import time
from typing import Iterator

POLL_INTERVAL_SECONDS = 60
NOTICE_TTL_SECONDS = 7 * 24 * 60 * 60
MAX_SEEN_NOTICES = 128
MAX_ACTORS = 64
MAX_STATE_BYTES = 96 * 1024
MAX_RENDERED_BYTES = 320
MAX_METADATA_BYTES = 320
TASK_REFERENCE = re.compile(r"P[1-9][0-9]*\.[1-9][0-9]*\Z")
ENTRY_ID = re.compile(r"E[1-9][0-9]*\Z")
SAFE_KIND = re.compile(r"[a-z][a-z0-9_-]{0,23}\Z")
SAFE_MODEL = re.compile(r"[A-Za-z0-9._:+/-]{1,32}\Z")
GENERATION = re.compile(r"[0-9a-f]{16}\Z")


def pending_reply_from_json(value: object) -> dict | None:
    """Find the bounded PendingReply inside a successful BoardReply JSON value."""
    if not isinstance(value, dict):
        return None
    if (isinstance(value.get("notices"), list)
            and isinstance(value.get("generation"), str)
            and "through" in value):
        return value
    if value.get("error") is not None:
        return None
    if value.get("status") not in (None, "ok"):
        return None
    result = value.get("result")
    if isinstance(result, dict):
        data = result.get("data")
        if isinstance(data, dict) and result.get("result") == "pending":
            return pending_reply_from_json(data)
        found = pending_reply_from_json(result)
        if found is not None:
            return found
    for key in ("data", "pending", "reply"):
        child = value.get(key)
        if isinstance(child, dict):
            found = pending_reply_from_json(child)
            if found is not None:
                return found
    return None


def pending_after(actor: str, state_dir: Path) -> int | None:
    """Return the notifier watermark for pagination, never the inbox cursor."""
    if not _valid_actor(actor):
        return None
    try:
        with _locked_state(state_dir) as state:
            row = state["watermarks"].get(actor)
            sequence = _positive_sequence(row.get("seq")) if isinstance(row, dict) else None
            return sequence if sequence and sequence > 0 else None
    except OSError:
        return None


def should_poll_pending(*, actor: str, state_dir: Path,
                        now: float | None = None) -> bool:
    """Atomically allow one pending poll per actor per 60 seconds."""
    if not _valid_actor(actor):
        return False
    current = time.time() if now is None else now
    try:
        with _locked_state(state_dir) as state:
            _prune_state(state, current)
            previous = state["polls"].get(actor)
            if isinstance(previous, (int, float)) and current - previous < POLL_INTERVAL_SECONDS:
                return False
            state["polls"][actor] = current
            _trim_mapping(state["polls"], MAX_ACTORS)
            return True
    except OSError:
        return False


def claim_pending_notices(*, actor: str, pending: dict,
                          state_dir: Path, now: float | None = None) -> list[dict]:
    """Reserve unseen actor/generation/entry notices and advance the local watermark."""
    if not _valid_actor(actor) or not isinstance(pending, dict):
        return []
    current = time.time() if now is None else now
    generation = _safe_text(pending.get("generation"), 96)
    if not GENERATION.fullmatch(generation):
        return []
    notices = pending.get("notices")
    notices = notices if isinstance(notices, list) else []
    try:
        with _locked_state(state_dir) as state:
            _prune_state(state, current)
            seen = state["seen"]
            existing = {(row.get("actor"), row.get("generation"), row.get("id"))
                        for row in seen if isinstance(row, dict)}
            fresh: list[dict] = []
            added: list[dict] = []
            received: list[int] = []
            for notice in notices[:MAX_SEEN_NOTICES]:
                if not isinstance(notice, dict):
                    continue
                view = _notice_view(notice, pending)
                if view is None:
                    continue
                if view.get("seq") is not None:
                    received.append(view["seq"])
                identity = (actor, generation, view["_dedupe_id"])
                if identity in existing:
                    continue
                existing.add(identity)
                fresh.append(view)
                added.append({"actor": actor, "generation": generation,
                              "id": view["_dedupe_id"], "seq": view["seq"], "seen_at": current})
            seen.extend(added)
            del seen[:-MAX_SEEN_NOTICES]

            through = _positive_sequence(pending.get("through"))
            omitted = _positive_sequence(pending.get("omitted")) or 0
            if omitted:
                candidate = max(received, default=None)
            else:
                candidate = through
            if candidate is not None:
                old = state["watermarks"].get(actor)
                old_seq = _positive_sequence(old.get("seq")) if isinstance(old, dict) else None
                state["watermarks"][actor] = {"seq": max(candidate, old_seq or 0), "updated_at": current}
                _trim_mapping(state["watermarks"], MAX_ACTORS)
            return fresh
    except OSError:
        return []


def render_pending_context(*, actor: str, pending: dict,
                           state_dir: Path, now: float | None = None) -> str | None:
    """Reserve unseen notices and render a compact, text-free hook context."""
    fresh = claim_pending_notices(actor=actor, pending=pending, state_dir=state_dir, now=now)
    if not fresh:
        return None
    labels = []
    for notice in fresh[:3]:
        label = notice.get("entry_id") or "Board update"
        kind = notice.get("kind")
        if kind:
            label += f" {kind}"
        task = notice.get("task")
        if task:
            label += f" on {task}"
        model = notice.get("model")
        if model:
            label += f" ({model}"
            age = notice.get("age")
            label += f", {age}" if age else ""
            label += ")"
        elif notice.get("age"):
            label += f" ({notice['age']})"
        labels.append(label)
    extra = len(fresh) - len(labels)
    if extra:
        labels.append(f"+{extra} more")
    rendered = "Board: " + "; ".join(labels) + ". Run trufflepig-agent board pending"
    return _bounded_text(rendered, MAX_RENDERED_BYTES)


def pending_metadata(*, pending: dict, notices: list[dict]) -> dict | None:
    """Build an 80-token-scale JSON projection without entry or event text."""
    if not notices:
        return None
    generation = _safe_text(pending.get("generation"), 32)
    result = {
        "generation": generation,
        "through": _positive_sequence(pending.get("through")),
        "notices": [
            {key: value for key, value in notice.items()
             if key in {"seq", "kind", "entry_id", "task"} and value is not None}
            for notice in notices[:3]
        ],
    }
    omitted = _positive_sequence(pending.get("omitted")) or 0
    if omitted or len(notices) > 3:
        result["omitted"] = omitted + max(0, len(notices) - 3)
    while len(json.dumps(result, ensure_ascii=True, separators=(",", ":")).encode()) > MAX_METADATA_BYTES:
        if result["notices"]:
            result["notices"].pop()
            result["omitted"] = result.get("omitted", 0) + 1
        elif result.get("generation"):
            result["generation"] = result["generation"][:12]
        else:
            return None
    return result


@contextmanager
def _locked_state(state_dir: Path) -> Iterator[dict]:
    directory = Path(state_dir)
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    directory_info = directory.lstat()
    if not stat.S_ISDIR(directory_info.st_mode) or directory_info.st_uid != os.getuid():
        raise OSError("notice state directory is not a private user-owned directory")
    os.chmod(directory, 0o700)
    lock_path = directory / ".notices.lock"
    flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    lock_fd = os.open(lock_path, flags, 0o600)
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        state = _read_state(directory / "notices.json")
        _normalize_state(state)
        yield state
        _write_state(directory, state)
    finally:
        fcntl.flock(lock_fd, fcntl.LOCK_UN)
        os.close(lock_fd)


def _read_state(path: Path) -> dict:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except OSError:
        return {}
    try:
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_size > MAX_STATE_BYTES):
            return {}
        with os.fdopen(fd, "rb", closefd=False) as handle:
            value = json.loads(handle.read(MAX_STATE_BYTES + 1))
        return value if isinstance(value, dict) else {}
    except (OSError, ValueError, UnicodeDecodeError):
        return {}
    finally:
        os.close(fd)


def _write_state(directory: Path, state: dict) -> None:
    payload = json.dumps(state, separators=(",", ":"), ensure_ascii=True).encode("utf-8")
    if len(payload) > MAX_STATE_BYTES:
        state["seen"] = state["seen"][-MAX_SEEN_NOTICES // 2:]
        _trim_mapping(state["polls"], MAX_ACTORS // 2)
        _trim_mapping(state["watermarks"], MAX_ACTORS // 2)
        payload = json.dumps(state, separators=(",", ":"), ensure_ascii=True).encode("utf-8")
    if len(payload) > MAX_STATE_BYTES:
        raise OSError("notice state cache exceeds its size limit")
    fd, temporary = tempfile.mkstemp(prefix=".notices-", dir=directory)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, directory / "notices.json")
        dir_fd = os.open(directory, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def _normalize_state(state: dict) -> None:
    for key in ("polls", "watermarks"):
        if not isinstance(state.get(key), dict):
            state[key] = {}
    if not isinstance(state.get("seen"), list):
        state["seen"] = []


def _prune_state(state: dict, now: float) -> None:
    cutoff = now - NOTICE_TTL_SECONDS
    state["seen"] = [row for row in state["seen"]
                     if isinstance(row, dict) and isinstance(row.get("seen_at"), (int, float))
                     and row["seen_at"] >= cutoff][-MAX_SEEN_NOTICES:]
    for actor, row in list(state["watermarks"].items()):
        if not isinstance(row, dict) or not isinstance(row.get("updated_at"), (int, float)) \
                or row["updated_at"] < cutoff:
            del state["watermarks"][actor]
    for actor, timestamp in list(state["polls"].items()):
        if not isinstance(timestamp, (int, float)) or timestamp < cutoff:
            del state["polls"][actor]


def _trim_mapping(mapping: dict, maximum: int) -> None:
    if len(mapping) <= maximum:
        return
    # Dict insertion order follows updates; remove oldest actors first.
    for actor in list(mapping)[:len(mapping) - maximum]:
        del mapping[actor]


def _notice_view(notice: dict, pending: dict) -> dict | None:
    payload = notice.get("entry")
    payload_kind = "entry"
    if not isinstance(payload, dict):
        payload = notice.get("event")
        payload_kind = "event"
    if not isinstance(payload, dict):
        return None
    seq = _positive_sequence(notice.get("seq")) or _positive_sequence(payload.get("seq"))
    entry_id = _safe_text(payload.get("id"), 64) if payload_kind == "entry" else ""
    if payload_kind == "entry" and not ENTRY_ID.fullmatch(entry_id):
        return None
    dedupe_id = entry_id or (f"event:{seq}" if seq is not None else "")
    if not dedupe_id:
        return None
    view = {
        "seq": seq,
        "kind": _safe_label(payload.get("kind"), 24, SAFE_KIND) or None,
        "entry_id": entry_id or None,
        "task": _task_reference(payload.get("refs")),
        "model": _safe_label(payload.get("model"), 32, SAFE_MODEL) or None,
        "age": _age_label(payload.get("created_at"), pending.get("server_now")),
    }
    # Event records have no EntryId. Sequence plus generation is their stable identity.
    view["_dedupe_id"] = dedupe_id
    return view


def _task_reference(value: object) -> str | None:
    if isinstance(value, str):
        return value if TASK_REFERENCE.fullmatch(value) else None
    if isinstance(value, list):
        for item in value:
            found = _task_reference(item)
            if found:
                return found
    if isinstance(value, dict):
        for item in value.values():
            found = _task_reference(item)
            if found:
                return found
    return None


def _age_label(created_at: object, server_now: object) -> str | None:
    created = _positive_sequence(created_at)
    now = _positive_sequence(server_now)
    if created is None or now is None or now < created:
        return None
    age = now - created
    if now > 10**12:
        age //= 1000
    if age < 60:
        return "under 1m"
    if age < 3600:
        return f"{age // 60}m"
    if age < 86400:
        return f"{age // 3600}h"
    return f"{age // 86400}d"


def _positive_sequence(value: object) -> int | None:
    if type(value) is int and 0 <= value < 2**63:
        return value
    if isinstance(value, str) and value.isdecimal() and len(value) <= 19:
        try:
            number = int(value)
            return number if number < 2**63 else None
        except ValueError:
            return None
    return None


def _safe_text(value: object, maximum: int) -> str:
    if not isinstance(value, str):
        return ""
    text = " ".join(value.split())
    return text.encode("utf-8")[:maximum].decode("utf-8", "ignore")


def _safe_label(value: object, maximum: int, pattern: re.Pattern) -> str:
    text = _safe_text(value, maximum)
    return text if pattern.fullmatch(text) else ""


def _bounded_text(value: str, maximum: int) -> str:
    encoded = value.encode("utf-8")
    if len(encoded) <= maximum:
        return value
    return encoded[:maximum - 1].decode("utf-8", "ignore").rstrip() + "…"


def _valid_actor(actor: str) -> bool:
    return isinstance(actor, str) and 0 < len(actor.encode("utf-8")) <= 160 and "\x00" not in actor
