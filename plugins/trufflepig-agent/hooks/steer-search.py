#!/usr/bin/env python3
"""PreToolUse hook: steer Grep/Glob/shell searches toward trufflepig-agent.

Usage: steer-search.py <kimi|muse>   (hook JSON payload on stdin)

Applies only inside repositories that are members of a registered trufflepig
workspace (`~/.config/trufflepig/*.toml`) or under a `trufflepig.workspace.toml`.
Modes via TRUFFLEPIG_AGENT_STEER:
  block  (default) exit 2 with a reason until trufflepig-agent has been used
         in this session for this directory, then allow;
  nudge  exit 0 and print a one-line reminder that is appended to context;
  off    exit 0 silently.
Every decision is appended to the agent audit log as verb "hook:steer".
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    tomllib = None

SEARCH_TOOLS = re.compile(r"^(grep|glob|ripgrep|rg|find_files|search_files|list_files)$", re.I)
SHELL_TOOLS = re.compile(r"^(bash|shell|run_command|execute|terminal)$", re.I)
SHELL_SEARCH = re.compile(r"(^|[\s;&|(])(rg|grep|egrep|fgrep|ag|ack|find)\s")
GRACE_SECONDS = 45 * 60


def state_dir() -> Path:
    base = os.environ.get("XDG_STATE_HOME") or os.path.join(os.path.expanduser("~"), ".local", "state")
    return Path(base) / "trufflepig"


def cwd_key(cwd: str) -> str:
    return hashlib.blake2s(cwd.encode(), digest_size=6).hexdigest()


def load_toml(path: Path) -> dict:
    try:
        document = tomllib.loads(path.read_text())
    except (OSError, ValueError):
        return {}
    return document if isinstance(document, dict) else {}


def member_roots_of(config_path: Path) -> list[Path]:
    """Member roots declared by one workspace config, relative to its directory."""
    roots: list[Path] = []
    for member in (load_toml(config_path).get("members") or {}).values():
        raw = member.get("path") if isinstance(member, dict) else None
        if raw:
            roots.append((config_path.parent / Path(os.path.expanduser(raw))).resolve())
    return roots


def workspace_roots() -> list[Path]:
    """Member roots of every workspace listed in the registry `workspaces.toml`."""
    if tomllib is None:
        return []
    config_dir = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "trufflepig"
    registry = config_dir / "workspaces.toml"
    entries = load_toml(registry).get("workspaces") or []
    roots: list[Path] = []
    for entry in entries:
        if isinstance(entry, str):
            roots.extend(member_roots_of((config_dir / Path(os.path.expanduser(entry))).resolve()))
    return roots


def git_common_dir(directory: Path) -> Path | None:
    """Canonical Git common directory of `directory`, or None outside a repository."""
    try:
        completed = subprocess.run(
            ["git", "-C", str(directory), "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, timeout=2, check=False,
            env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0 or not completed.stdout.strip():
        return None
    return Path(completed.stdout.strip()).resolve()


def member_common_dir(root: Path) -> Path | None:
    dot_git = root / ".git"
    if dot_git.is_dir():
        return dot_git.resolve()
    return git_common_dir(root) if dot_git.is_file() else None


def indexed_root(cwd: Path) -> Path | None:
    """The member root that owns `cwd`: by path, then by Git common directory so a
    linked worktree anywhere on disk maps to its member."""
    roots = workspace_roots()
    for root in roots:
        if cwd == root or root in cwd.parents:
            return root
    for ancestor in (cwd, *cwd.parents):
        if (ancestor / "trufflepig.workspace.toml").is_file():
            return ancestor
    if not any((ancestor / ".git").is_file() for ancestor in (cwd, *cwd.parents)):
        return None
    common = git_common_dir(cwd)
    if common is None:
        return None
    for root in roots:
        if member_common_dir(root) == common:
            return root
    return None


def session_used_tool(harness: str, cwd: str, session: str) -> bool:
    recent = state_dir() / "agent-recent" / harness
    if not recent.is_dir():
        return False
    key = cwd_key(cwd)
    now = time.time()
    # Any session's recent call for this directory counts within the grace window.
    del session
    for path in recent.glob(f"{key}-*"):
        try:
            if now - path.stat().st_mtime <= GRACE_SECONDS:
                return True
        except OSError:
            continue
    return False


def wants_search(tool: str, tool_input: dict) -> str | None:
    if SEARCH_TOOLS.match(tool or ""):
        return tool
    if SHELL_TOOLS.match(tool or ""):
        command = tool_input.get("command") or tool_input.get("cmd") or ""
        if isinstance(command, list):
            command = " ".join(map(str, command))
        if SHELL_SEARCH.search(str(command)):
            return f"{tool}:{str(command).split()[0] if str(command).split() else ''}"
    return None


def log(record: dict) -> None:
    try:
        target = Path(os.environ.get("TRUFFLEPIG_AGENT_LOG_DIR") or state_dir() / "agent-audit")
        target.mkdir(parents=True, exist_ok=True)
        with (target / f"{record['harness']}.jsonl").open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, separators=(",", ":")) + "\n")
    except OSError:
        pass


def main() -> int:
    harness = sys.argv[1] if len(sys.argv) > 1 else "unknown"
    mode = os.environ.get("TRUFFLEPIG_AGENT_STEER", "block").lower()
    if mode == "off":
        return 0
    try:
        payload = json.load(sys.stdin)
    except ValueError:
        return 0
    if not isinstance(payload, dict):
        return 0
    tool = payload.get("tool_name") or payload.get("tool") or payload.get("name") or ""
    tool_input = payload.get("tool_input") or payload.get("input") or payload.get("arguments") or {}
    if not isinstance(tool_input, dict):
        tool_input = {}
    matched = wants_search(str(tool), tool_input)
    if not matched:
        return 0
    cwd = str(Path(payload.get("cwd") or os.getcwd()).resolve())
    root = indexed_root(Path(cwd))
    if root is None:
        return 0
    session = str(payload.get("session_id") or payload.get("sessionId") or "")
    used = session_used_tool(harness, cwd, session)
    decision = "allow" if used or mode != "block" else "block"
    if mode == "nudge" and not used:
        decision = "nudge"
    pattern = tool_input.get("pattern") or tool_input.get("query") or ""
    if not pattern and tool_input.get("command"):
        command = tool_input["command"]
        words = command.split() if isinstance(command, str) else [str(w) for w in command]
        # First non-flag word after the search program is the pattern.
        positional = [w for w in words[1:] if not w.startswith("-")]
        pattern = positional[0] if positional else ""
    pattern = str(pattern).strip("'\"")
    hint = f"trufflepig-agent search '{pattern[:60]}'" if pattern else "trufflepig-agent search 'terms'"
    log({
        "ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "harness": harness,
        "session": session or f"{harness}-{cwd_key(cwd)}-{time.strftime('%Y%m%d')}",
        "cwd": cwd,
        "verb": "hook:steer",
        "args": [matched, str(pattern)[:120]],
        "options": {"mode": mode},
        "exit_code": 2 if decision == "block" else 0,
        "elapsed_ms": 0,
        "status": decision,
        "error": "",
        "stderr": "",
        "hits": None,
        "truncated": None,
        "has_next": False,
        "stdout_bytes": 0,
        "coverage": {},
        "repeat_count": 0,
        "signals": ["steer_block"] if decision == "block" else (["steer_nudge"] if decision == "nudge" else []),
    })
    message = (
        f"This repository ({root.name}) is indexed by trufflepig. Run `{hint}` first "
        "(ranked, reranked, with handles for `show` and `ctx`); Grep/Glob/rg are allowed "
        "again after one trufflepig-agent call in this session. Use `re:pattern` for regex."
    )
    if decision == "block":
        sys.stderr.write(message + "\n")
        return 2
    if decision == "nudge":
        sys.stdout.write(message + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
