"""Merge the Claude session hook and narrow runtime access into user settings."""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import shlex
import stat
import tempfile


def claude_home() -> Path:
    return Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude").expanduser().absolute()


def prepare_settings(path: Path, hook: Path, runtime: Path) -> dict:
    settings = json.loads(path.read_text()) if path.exists() else {}
    if not isinstance(settings, dict):
        raise ValueError(f"expected a settings object in {path}")
    settings = copy.deepcopy(settings)
    if settings.get("disableAllHooks"):
        raise ValueError("Claude disables hooks; enable them before installing session attribution")
    command = shlex.quote(str(hook.absolute()))
    hooks = settings.setdefault("hooks", {})
    if not isinstance(hooks, dict):
        raise ValueError("Claude hooks must be an object")
    start = hooks.setdefault("SessionStart", [])
    if not isinstance(start, list):
        raise ValueError("Claude SessionStart hooks must be an array")
    installed = any(isinstance(group, dict) and not group.get("matcher") and
                    any(h.get("type") == "command" and h.get("command") == command
                        for h in group.get("hooks", []) if isinstance(h, dict)) for group in start)
    if not installed:
        start.append({"hooks": [{"type": "command", "command": command, "timeout": 5}]})
    sandbox = settings.setdefault("sandbox", {})
    if not isinstance(sandbox, dict):
        raise ValueError("Claude sandbox must be an object")
    filesystem = sandbox.setdefault("filesystem", {})
    if not isinstance(filesystem, dict):
        raise ValueError("Claude sandbox.filesystem must be an object")
    writable = filesystem.setdefault("allowWrite", [])
    if not isinstance(writable, list) or not all(isinstance(p, str) for p in writable):
        raise ValueError("Claude sandbox.filesystem.allowWrite must be a string array")
    if str(runtime) not in writable:
        writable.append(str(runtime))
    return settings


def write_settings(path: Path, settings: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    mode = stat.S_IMODE(path.stat().st_mode) if path.exists() else 0o600
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=".trufflepig-settings-", delete=False) as output:
            temporary = Path(output.name)
            output.write(json.dumps(settings, indent=2, ensure_ascii=False) + "\n")
        temporary.chmod(mode)
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
